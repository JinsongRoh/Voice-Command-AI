//! Checks and application of edits to the managed list. Pure: the service
//! stores the result. Removing an entry never touches a running window.

use std::path::{Path, PathBuf};

use vca_core::error::ErrorCode;
use vca_core::{
    Error, MAX_RESTORE_DELAY_SECS, MAX_RESTORE_PROFILE_CHARS, MAX_RESTORE_PROMPT_CHARS,
    MAX_RESTORE_TITLE_CHARS, RestoreEdit, RestoreEntry, RestoreEntryDraft, RestoreEntrySource,
    RestoreProfile, UnixMillis,
};

use super::capture::next_order;
use super::resume::{agent_program, valid_session_id};
use super::store::RestoreFile;

/// Most profiles on a computer.
const MAX_PROFILES: usize = 20;
/// Most entries in a profile.
const MAX_ENTRIES: usize = 200;

fn invalid(message: impl Into<String>) -> Error {
    Error::api(ErrorCode::InvalidRequest, message)
}

fn not_found(what: &str, key: &str) -> Error {
    Error::api(ErrorCode::NotFound, format!("no {what} {key}"))
}

/// A window title: trimmed, 1-80 characters, no control characters and no
/// invisible format characters (bidirectional overrides, zero-width
/// characters, line separators) that could disguise it. Quotes are fine:
/// command lines quote every title they carry.
pub fn checked_title(title: &str) -> Result<String, Error> {
    let title = title.trim();
    if title.is_empty() {
        return Err(invalid("the window title is empty"));
    }
    if title
        .chars()
        .any(vca_core::powershell::is_unsafe_title_char)
    {
        return Err(invalid(
            "the title must not contain control or invisible formatting characters",
        ));
    }
    if title.chars().count() > MAX_RESTORE_TITLE_CHARS {
        return Err(invalid(format!(
            "the title is longer than {MAX_RESTORE_TITLE_CHARS} characters"
        )));
    }
    Ok(title.to_owned())
}

fn checked_profile_name(name: &str) -> Result<String, Error> {
    let name = name.trim();
    if name.is_empty()
        || name.chars().count() > MAX_RESTORE_PROFILE_CHARS
        || name.chars().any(char::is_control)
    {
        return Err(invalid(format!(
            "a profile name has 1 to {MAX_RESTORE_PROFILE_CHARS} characters"
        )));
    }
    Ok(name.to_owned())
}

/// The folder of an entry must be an absolute path (of the computer that
/// runs the restore, whatever OS the client has).
pub fn absolute_folder(folder: &Path) -> Result<PathBuf, Error> {
    let text = folder.to_string_lossy();
    let bytes = text.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\');
    if drive || text.starts_with('/') || text.starts_with(r"\\") {
        Ok(folder.to_owned())
    } else {
        Err(invalid(format!(
            "the folder must be an absolute path: {text}"
        )))
    }
}

/// The message typed after a restore: one line, no double quotes (it goes
/// through two command lines), and not starting with `-`: it is the agent's
/// last argument, so it would be read as a flag.
fn checked_prompt(prompt: Option<&str>) -> Result<Option<String>, Error> {
    let Some(prompt) = prompt.map(str::trim).filter(|p| !p.is_empty()) else {
        return Ok(None);
    };
    if prompt.starts_with('-') {
        return Err(invalid(
            "the message cannot start with '-': the agent would read it as an option",
        ));
    }
    if prompt.chars().count() > MAX_RESTORE_PROMPT_CHARS
        || prompt.chars().any(|c| c.is_control() || c == '"')
    {
        return Err(invalid(format!(
            "the message is one line of at most {MAX_RESTORE_PROMPT_CHARS} characters, without double quotes"
        )));
    }
    Ok(Some(prompt.to_owned()))
}

fn checked_draft(draft: &RestoreEntryDraft) -> Result<RestoreEntryDraft, Error> {
    if agent_program(&draft.agent).is_none() {
        return Err(invalid(
            "only known agents can be restored, not plain shells or other programs",
        ));
    }
    if let Some(id) = &draft.session_id {
        if !valid_session_id(id) {
            return Err(invalid("that is not a session id"));
        }
    }
    if let Some(rect) = &draft.rect {
        if rect.w <= 0 || rect.h <= 0 {
            return Err(invalid("the window has no size"));
        }
    }
    Ok(RestoreEntryDraft {
        title: checked_title(&draft.title)?,
        folder: absolute_folder(&draft.folder)?,
        ..draft.clone()
    })
}

/// Finds a profile by id, else by name (ignoring case).
pub fn find_profile<'a>(file: &'a RestoreFile, key: &str) -> Option<&'a RestoreProfile> {
    file.profiles.iter().find(|p| p.id == key).or_else(|| {
        file.profiles
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(key))
    })
}

fn profile_mut<'a>(file: &'a mut RestoreFile, key: &str) -> Result<&'a mut RestoreProfile, Error> {
    let id = find_profile(file, key)
        .map(|p| p.id.clone())
        .ok_or_else(|| not_found("profile", key))?;
    file.profiles
        .iter_mut()
        .find(|p| p.id == id)
        .ok_or_else(|| not_found("profile", key))
}

fn title_taken(profile: &RestoreProfile, title: &str, except: Option<&str>) -> bool {
    profile
        .entries
        .iter()
        .any(|e| e.title == title && except != Some(e.id.as_str()))
}

fn duplicate_title(title: &str) -> Error {
    Error::api(
        ErrorCode::Conflict,
        format!("another window is already named {title}: titles are how windows are found"),
    )
}

/// Applies one edit. `local` is true for the owner on this computer: only
/// then can the "type after restore" message change.
pub fn apply_edit(
    file: &mut RestoreFile,
    edit: &RestoreEdit,
    local: bool,
    now: UnixMillis,
) -> Result<(), Error> {
    match edit {
        RestoreEdit::ProfileAdd { name } => {
            let name = checked_profile_name(name)?;
            if file.profiles.len() >= MAX_PROFILES {
                return Err(invalid(format!("at most {MAX_PROFILES} profiles")));
            }
            if find_profile(file, &name).is_some() {
                return Err(Error::api(
                    ErrorCode::Conflict,
                    format!("a profile named {name} exists"),
                ));
            }
            file.profiles.push(RestoreProfile {
                id: format!("p{}", uuid::Uuid::new_v4().simple()),
                name,
                delay_secs: vca_core::DEFAULT_RESTORE_DELAY_SECS,
                auto_prompt: None,
                entries: Vec::new(),
            });
        }
        RestoreEdit::ProfileRename { profile, name } => {
            let name = checked_profile_name(name)?;
            profile_mut(file, profile)?.name = name;
        }
        RestoreEdit::ProfileDelete { profile } => {
            let id = profile_mut(file, profile)?.id.clone();
            if file.profiles.len() == 1 {
                return Err(Error::api(
                    ErrorCode::Conflict,
                    "the last profile cannot be deleted",
                ));
            }
            file.profiles.retain(|p| p.id != id);
            if file.active == id {
                file.active.clone_from(&file.profiles[0].id);
            }
        }
        RestoreEdit::ProfileActivate { profile } => {
            file.active = profile_mut(file, profile)?.id.clone();
        }
        RestoreEdit::ProfileSettings {
            profile,
            delay_secs,
            auto_prompt,
        } => {
            if *delay_secs > MAX_RESTORE_DELAY_SECS {
                return Err(invalid(format!(
                    "the pause is at most {MAX_RESTORE_DELAY_SECS} seconds"
                )));
            }
            let prompt = checked_prompt(auto_prompt.as_deref())?;
            let target = profile_mut(file, profile)?;
            if !local && prompt != target.auto_prompt {
                return Err(Error::api(
                    ErrorCode::Forbidden,
                    "the message after restore can only be changed on the computer itself",
                ));
            }
            target.delay_secs = *delay_secs;
            target.auto_prompt = prompt;
        }
        RestoreEdit::EntryAdd { profile, entry } => {
            let draft = checked_draft(entry)?;
            let target = profile_mut(file, profile)?;
            if target.entries.len() >= MAX_ENTRIES {
                return Err(invalid(format!(
                    "at most {MAX_ENTRIES} windows in a profile"
                )));
            }
            if title_taken(target, &draft.title, None) {
                return Err(duplicate_title(&draft.title));
            }
            let order = next_order(target);
            target.entries.push(RestoreEntry {
                id: format!("e{}", uuid::Uuid::new_v4().simple()),
                title: draft.title,
                agent: draft.agent,
                folder: draft.folder,
                session_id: draft.session_id,
                rect: draft.rect,
                monitor: None,
                maximized: draft.maximized,
                enabled: draft.enabled,
                order,
                args: Vec::new(),
                transcript: None,
                note: None,
                source: RestoreEntrySource::Manual,
                updated_at: now,
            });
        }
        RestoreEdit::EntryUpdate {
            profile,
            entry_id,
            entry,
        } => {
            let draft = checked_draft(entry)?;
            let target = profile_mut(file, profile)?;
            if title_taken(target, &draft.title, Some(entry_id)) {
                return Err(duplicate_title(&draft.title));
            }
            let saved = target
                .entries
                .iter_mut()
                .find(|e| e.id == *entry_id)
                .ok_or_else(|| not_found("window", entry_id))?;
            if saved.agent != draft.agent {
                // Captured arguments belong to the old agent.
                saved.args.clear();
                saved.transcript = None;
            }
            if saved.rect != draft.rect {
                saved.monitor = None;
            }
            saved.title = draft.title;
            saved.agent = draft.agent;
            saved.folder = draft.folder;
            saved.session_id = draft.session_id;
            saved.rect = draft.rect;
            saved.maximized = draft.maximized;
            saved.enabled = draft.enabled;
            saved.updated_at = now;
        }
        RestoreEdit::EntryDelete { profile, entry_id } => {
            let target = profile_mut(file, profile)?;
            let before = target.entries.len();
            target.entries.retain(|e| e.id != *entry_id);
            if target.entries.len() == before {
                return Err(not_found("window", entry_id));
            }
        }
        RestoreEdit::EntrySetEnabled {
            profile,
            entry_id,
            enabled,
        } => {
            let target = profile_mut(file, profile)?;
            let saved = target
                .entries
                .iter_mut()
                .find(|e| e.id == *entry_id)
                .ok_or_else(|| not_found("window", entry_id))?;
            saved.enabled = *enabled;
            saved.updated_at = now;
        }
        RestoreEdit::EntryReorder { profile, entry_ids } => {
            let target = profile_mut(file, profile)?;
            let mut wanted = entry_ids.clone();
            wanted.sort();
            let mut have: Vec<String> = target.entries.iter().map(|e| e.id.clone()).collect();
            have.sort();
            if wanted != have {
                return Err(invalid("the new order must list every window once"));
            }
            for (order, id) in entry_ids.iter().enumerate() {
                if let Some(saved) = target.entries.iter_mut().find(|e| e.id == *id) {
                    saved.order = order as u32;
                }
            }
            target.entries.sort_by_key(|e| e.order);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use vca_core::AgentKind;

    use super::super::store::DEFAULT_PROFILE;
    use super::*;

    fn draft(title: &str) -> RestoreEntryDraft {
        RestoreEntryDraft {
            title: title.into(),
            agent: AgentKind::ClaudeCode,
            folder: PathBuf::from("E:\\work"),
            session_id: None,
            rect: None,
            maximized: false,
            enabled: true,
        }
    }

    fn add(file: &mut RestoreFile, title: &str) -> String {
        apply_edit(
            file,
            &RestoreEdit::EntryAdd {
                profile: DEFAULT_PROFILE.into(),
                entry: draft(title),
            },
            true,
            1,
        )
        .unwrap();
        file.profiles[0].entries.last().unwrap().id.clone()
    }

    fn code(result: Result<(), Error>) -> ErrorCode {
        result.unwrap_err().code()
    }

    #[test]
    fn profiles_are_added_renamed_activated_and_deleted() {
        let mut file = RestoreFile::default();
        let add = RestoreEdit::ProfileAdd {
            name: " Home ".into(),
        };
        apply_edit(&mut file, &add, true, 1).unwrap();
        assert_eq!(file.profiles[1].name, "Home");
        assert_eq!(file.profiles[1].delay_secs, 6);
        assert_eq!(
            code(apply_edit(&mut file, &add, true, 1)),
            ErrorCode::Conflict
        );
        apply_edit(
            &mut file,
            &RestoreEdit::ProfileActivate {
                profile: "home".into(),
            },
            true,
            1,
        )
        .unwrap();
        let id = file.profiles[1].id.clone();
        assert_eq!(file.active, id);
        apply_edit(
            &mut file,
            &RestoreEdit::ProfileRename {
                profile: id.clone(),
                name: "Lab".into(),
            },
            true,
            1,
        )
        .unwrap();
        // Deleting the active profile falls back to the first one.
        apply_edit(
            &mut file,
            &RestoreEdit::ProfileDelete { profile: id },
            true,
            1,
        )
        .unwrap();
        assert_eq!(file.active, DEFAULT_PROFILE);
        let last = RestoreEdit::ProfileDelete {
            profile: DEFAULT_PROFILE.into(),
        };
        assert_eq!(
            code(apply_edit(&mut file, &last, true, 1)),
            ErrorCode::Conflict
        );
    }

    #[test]
    fn entries_are_added_updated_toggled_reordered_and_deleted() {
        let mut file = RestoreFile::default();
        let a = add(&mut file, "A");
        let b = add(&mut file, "B");
        let c = add(&mut file, "C");
        assert_eq!(
            file.profiles[0]
                .entries
                .iter()
                .map(|e| e.order)
                .collect::<Vec<_>>(),
            [0, 1, 2]
        );
        apply_edit(
            &mut file,
            &RestoreEdit::EntryReorder {
                profile: DEFAULT_PROFILE.into(),
                entry_ids: vec![c.clone(), a.clone(), b.clone()],
            },
            true,
            2,
        )
        .unwrap();
        let titles: Vec<&str> = file.profiles[0]
            .entries
            .iter()
            .map(|e| e.title.as_str())
            .collect();
        assert_eq!(titles, ["C", "A", "B"]);
        apply_edit(
            &mut file,
            &RestoreEdit::EntrySetEnabled {
                profile: DEFAULT_PROFILE.into(),
                entry_id: a.clone(),
                enabled: false,
            },
            true,
            3,
        )
        .unwrap();
        assert!(!file.profiles[0].entries[1].enabled);
        let mut updated = draft("A2");
        updated.session_id = Some("s-1".into());
        apply_edit(
            &mut file,
            &RestoreEdit::EntryUpdate {
                profile: DEFAULT_PROFILE.into(),
                entry_id: a.clone(),
                entry: updated,
            },
            true,
            4,
        )
        .unwrap();
        assert_eq!(file.profiles[0].entries[1].title, "A2");
        assert_eq!(
            file.profiles[0].entries[1].session_id.as_deref(),
            Some("s-1")
        );
        apply_edit(
            &mut file,
            &RestoreEdit::EntryDelete {
                profile: DEFAULT_PROFILE.into(),
                entry_id: a,
            },
            true,
            5,
        )
        .unwrap();
        assert_eq!(file.profiles[0].entries.len(), 2);
    }

    #[test]
    fn a_partial_reorder_is_refused() {
        let mut file = RestoreFile::default();
        let a = add(&mut file, "A");
        add(&mut file, "B");
        let edit = RestoreEdit::EntryReorder {
            profile: DEFAULT_PROFILE.into(),
            entry_ids: vec![a],
        };
        assert_eq!(
            code(apply_edit(&mut file, &edit, true, 1)),
            ErrorCode::InvalidRequest
        );
    }

    #[test]
    fn titles_reject_invisible_characters_but_keep_quotes() {
        assert_eq!(
            checked_title(" Bob\u{2019}s 'fix' ").unwrap(),
            "Bob\u{2019}s 'fix'"
        );
        for bad in [
            "a\nb",
            "a\u{202E}b",
            "a\u{200B}b",
            "a\u{2028}b",
            "\u{FEFF}a",
        ] {
            assert!(checked_title(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn titles_stay_unique_in_a_profile() {
        let mut file = RestoreFile::default();
        add(&mut file, "A");
        let b = add(&mut file, "B");
        let edit = RestoreEdit::EntryUpdate {
            profile: DEFAULT_PROFILE.into(),
            entry_id: b,
            entry: draft("A"),
        };
        assert_eq!(
            code(apply_edit(&mut file, &edit, true, 1)),
            ErrorCode::Conflict
        );
    }

    #[test]
    fn only_known_agents_and_clean_values_are_accepted() {
        let mut file = RestoreFile::default();
        for bad in [
            RestoreEntryDraft {
                agent: AgentKind::Generic("powershell".into()),
                ..draft("x")
            },
            RestoreEntryDraft {
                agent: AgentKind::Shell("cmd".into()),
                ..draft("x")
            },
            RestoreEntryDraft {
                session_id: Some("id; rm -rf".into()),
                ..draft("x")
            },
            RestoreEntryDraft {
                folder: PathBuf::from("relative\\path"),
                ..draft("x")
            },
            draft("a\nb"),
            draft("   "),
        ] {
            let edit = RestoreEdit::EntryAdd {
                profile: DEFAULT_PROFILE.into(),
                entry: bad,
            };
            assert_eq!(
                code(apply_edit(&mut file, &edit, true, 1)),
                ErrorCode::InvalidRequest
            );
        }
    }

    #[test]
    fn the_auto_prompt_changes_only_on_the_computer_itself() {
        let mut file = RestoreFile::default();
        assert_eq!(file.profiles[0].auto_prompt, None);
        let set = RestoreEdit::ProfileSettings {
            profile: DEFAULT_PROFILE.into(),
            delay_secs: 10,
            auto_prompt: Some("go on".into()),
        };
        assert_eq!(
            code(apply_edit(&mut file, &set, false, 1)),
            ErrorCode::Forbidden
        );
        assert_eq!(file.profiles[0].delay_secs, 6);
        apply_edit(&mut file, &set, true, 1).unwrap();
        assert_eq!(file.profiles[0].auto_prompt.as_deref(), Some("go on"));
        // A remote device can change the pause and leave the message alone.
        let delay_only = RestoreEdit::ProfileSettings {
            profile: DEFAULT_PROFILE.into(),
            delay_secs: 3,
            auto_prompt: Some("go on".into()),
        };
        apply_edit(&mut file, &delay_only, false, 1).unwrap();
        assert_eq!(file.profiles[0].delay_secs, 3);
        // Nor can it clear the message.
        let clear = RestoreEdit::ProfileSettings {
            profile: DEFAULT_PROFILE.into(),
            delay_secs: 3,
            auto_prompt: None,
        };
        assert_eq!(
            code(apply_edit(&mut file, &clear, false, 1)),
            ErrorCode::Forbidden
        );
    }

    #[test]
    fn delay_and_prompt_have_limits() {
        let mut file = RestoreFile::default();
        let long = RestoreEdit::ProfileSettings {
            profile: DEFAULT_PROFILE.into(),
            delay_secs: 121,
            auto_prompt: None,
        };
        assert_eq!(
            code(apply_edit(&mut file, &long, true, 1)),
            ErrorCode::InvalidRequest
        );
        for prompt in ["two\nlines", "a \"quoted\" word"] {
            let bad = RestoreEdit::ProfileSettings {
                profile: DEFAULT_PROFILE.into(),
                delay_secs: 1,
                auto_prompt: Some(prompt.into()),
            };
            assert_eq!(
                code(apply_edit(&mut file, &bad, true, 1)),
                ErrorCode::InvalidRequest
            );
        }
    }

    #[test]
    fn a_prompt_cannot_become_an_agent_flag() {
        for prompt in [
            "--dangerously-skip-permissions",
            "-p",
            "  --yolo",
            "-",
            "--",
        ] {
            let err = checked_prompt(Some(prompt)).unwrap_err();
            assert_eq!(err.code(), ErrorCode::InvalidRequest, "{prompt}");
        }
        let mut file = RestoreFile::default();
        let flag = RestoreEdit::ProfileSettings {
            profile: DEFAULT_PROFILE.into(),
            delay_secs: 1,
            auto_prompt: Some("--resume other".into()),
        };
        assert_eq!(
            code(apply_edit(&mut file, &flag, true, 1)),
            ErrorCode::InvalidRequest
        );
        assert_eq!(file.profiles[0].auto_prompt, None);
        // A dash later in the text is plain text.
        assert_eq!(
            checked_prompt(Some("go on - step 2 --fast")).unwrap(),
            Some("go on - step 2 --fast".to_owned())
        );
    }

    #[test]
    fn unknown_targets_are_not_found() {
        let mut file = RestoreFile::default();
        let edit = RestoreEdit::EntryDelete {
            profile: DEFAULT_PROFILE.into(),
            entry_id: "nope".into(),
        };
        assert_eq!(
            code(apply_edit(&mut file, &edit, true, 1)),
            ErrorCode::NotFound
        );
        let edit = RestoreEdit::ProfileDelete {
            profile: "nope".into(),
        };
        assert_eq!(
            code(apply_edit(&mut file, &edit, true, 1)),
            ErrorCode::NotFound
        );
    }
}
