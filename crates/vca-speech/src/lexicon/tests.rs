use super::*;

fn lexicon_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lexicon")
}

fn base_engine() -> VocabularyEngine {
    VocabularyEngine::from_dir(&lexicon_dir(), None).expect("base lexicon loads")
}

fn small_engine() -> VocabularyEngine {
    VocabularyEngine::new(Lexicon::from_entries(
        Layer::Base,
        [
            TermEntry::new("kubectl")
                .variant("kube control", Some("en"))
                .variant("cube control", Some("en"))
                .variant("큐브 컨트롤", Some("ko"))
                .weight(1.5),
            TermEntry::new("useEffect")
                .variant("use effect", Some("en"))
                .variant("ユーズエフェクト", Some("ja"))
                .variant("유즈 이펙트", Some("ko")),
            TermEntry::new("Claude Code").variant("cloud code", Some("en")),
            TermEntry::new("GitHub").variant("git hub", Some("en")),
            TermEntry::new("Go").variant("golang", Some("en")),
            TermEntry::new("PostgreSQL").variant("postgres q l", Some("en")),
        ],
    ))
}

#[test]
fn base_lexicon_loads_and_is_large_enough() {
    let engine = base_engine();
    let en = Lexicon::load_dir(&lexicon_dir(), Some(&["en"])).unwrap();
    assert!(en.len() >= 300, "only {} English terms", en.len());
    for lang in ["ja", "ko", "zh", "es", "de", "fr"] {
        let l = Lexicon::load_dir(&lexicon_dir(), Some(&[lang])).unwrap();
        assert!(l.len() >= 40, "{lang} has only {} terms", l.len());
        // Localized entries must refer to terms that exist in English.
        for e in l.entries() {
            assert!(
                en.get(&e.term).is_some(),
                "{lang} entry {:?} has no English base entry",
                e.term
            );
        }
    }
    let kubectl = engine.base().get("kubectl").unwrap();
    assert!(
        kubectl
            .variants
            .iter()
            .any(|v| v.language.as_deref() == Some("ko"))
    );
    assert!(
        kubectl
            .variants
            .iter()
            .any(|v| v.language.as_deref() == Some("en"))
    );
}

#[test]
fn base_lexicon_homophones_have_a_clear_winner() {
    let engine = base_engine();
    for c in engine.conflicts() {
        if c.kind != ConflictKind::Homophone {
            continue;
        }
        let names: Vec<&str> = c.candidates.iter().map(|(t, _)| t.as_str()).collect();
        let a = engine.base().get(names[0]).unwrap().weight;
        let b = engine.base().get(names[1]).unwrap().weight;
        assert!(
            a != b,
            "variant {:?} maps to {:?} with equal weights; add a weight or remove the variant",
            c.key,
            names
        );
    }
}

#[test]
fn rejects_unknown_fields() {
    let mut lex = Lexicon::new(Layer::Base);
    let err = lex
        .parse_toml(
            "[[entry]]\nterm = \"x\"\nvariant = [\"y\"]\n",
            Some("en"),
            "t.toml",
        )
        .unwrap_err();
    assert!(matches!(err, SpeechError::Parse { .. }));
}

#[test]
fn corrects_multiword_variants_case_insensitively() {
    let engine = small_engine();
    let r = engine.correct("Run Kube Control get pods and fix the use effect hook");
    assert_eq!(r.text, "Run kubectl get pods and fix the useEffect hook");
    assert_eq!(r.corrections.len(), 2);
    let c = &r.corrections[0];
    assert_eq!(c.original, "Kube Control");
    assert_eq!(&r.text[c.corrected_start..c.corrected_end], "kubectl");
    assert_eq!(c.kind, CorrectionKind::Exact);
    assert_eq!(c.layer, Layer::Base);
}

#[test]
fn corrects_cjk_without_spaces_and_hangul_with_particles() {
    let engine = small_engine();
    let r = engine.correct("ユーズエフェクトを使ってください");
    assert_eq!(r.text, "useEffectを使ってください");
    let r = engine.correct("유즈 이펙트를 고쳐 주고 큐브 컨트롤로 확인해");
    assert_eq!(r.text, "useEffect를 고쳐 주고 kubectl로 확인해");
    // Mixed language in one sentence.
    let r = engine.correct("cloud code で kube control を実行");
    assert_eq!(r.text, "Claude Code で kubectl を実行");
}

#[test]
fn respects_word_boundaries() {
    let engine = small_engine();
    // "git hubs" is not "git hub"; "golangci" is not "golang".
    let r = engine.correct("the git hubs and golangci-lint");
    assert!(r.corrections.is_empty(), "{:?}", r.corrections);
}

#[test]
fn rewrites_casing_only_for_distinctive_terms() {
    let engine = small_engine();
    let r = engine.correct("push to github and go home");
    assert_eq!(r.text, "push to GitHub and go home");
}

#[test]
fn fuzzy_matching_is_conservative() {
    let engine = small_engine();
    let r = engine.correct("please run kube controll now");
    assert_eq!(r.text, "please run kubectl now");
    assert_eq!(r.corrections[0].kind, CorrectionKind::Fuzzy);
    assert!(r.corrections[0].score < 1.0);
    // Joined spoken form.
    let r = engine.correct("fix useeffect");
    assert_eq!(r.text, "fix useEffect");
    // Ordinary words close to nothing are untouched.
    for s in [
        "make the code faster",
        "open the cloud console",
        "use the effect system",
        "update the post",
    ] {
        let r = engine.correct(s);
        assert!(r.corrections.is_empty(), "{s:?} -> {:?}", r.corrections);
    }
    // Disabling fuzzy matching keeps exact matches only.
    let mut strict = small_engine();
    strict.set_config(CorrectionConfig {
        fuzzy: false,
        ..CorrectionConfig::default()
    });
    assert!(strict.correct("kube controll").corrections.is_empty());
}

#[test]
fn layer_priority_resolves_homophones() {
    let mut engine = small_engine();
    // The project defines its own "cloud code" (a folder named cloud-code).
    let project = ProjectLexicon {
        lexicon: Lexicon::from_entries(
            Layer::Project,
            [TermEntry::new("cloud-code").variant("cloud code", None)],
        ),
        ecosystems: vec![],
        current_branch: None,
    };
    engine.set_project(project);
    let r = engine.correct("open cloud code");
    assert_eq!(r.text, "open cloud-code");
    assert_eq!(r.corrections[0].layer, Layer::Project);
    assert_eq!(
        r.corrections[0].alternatives,
        vec!["Claude Code".to_owned()]
    );
    // Personal beats project.
    engine.add_personal_term(TermEntry::new("Claude Code").variant("cloud code", None));
    let r = engine.correct("open cloud code");
    assert_eq!(r.text, "open Claude Code");
    assert_eq!(r.corrections[0].layer, Layer::Personal);
    let conflicts = engine.conflicts();
    let c = conflicts
        .iter()
        .find(|c| c.kind == ConflictKind::Homophone && c.key == "cloud code")
        .unwrap();
    assert_eq!(c.winner, "Claude Code");
    assert_eq!(c.candidates.len(), 2);
}

#[test]
fn context_topics_break_ties() {
    let mut engine = VocabularyEngine::new(Lexicon::from_entries(
        Layer::Base,
        [
            TermEntry {
                tags: vec!["python".into()],
                ..TermEntry::new("pytest").variant("pie test", None)
            },
            TermEntry {
                tags: vec!["rust".into()],
                ..TermEntry::new("cargo test").variant("pie test", None)
            },
        ],
    ));
    engine.set_context(VocabContext {
        language: None,
        topics: vec!["rust".into()],
    });
    assert_eq!(engine.correct("run pie test").text, "run cargo test");
    engine.set_context(VocabContext {
        language: None,
        topics: vec!["python".into()],
    });
    assert_eq!(engine.correct("run pie test").text, "run pytest");
}

#[test]
fn near_duplicates_across_layers_are_reported() {
    let mut engine = small_engine();
    engine.set_project(ProjectLexicon {
        lexicon: Lexicon::from_entries(Layer::Project, [TermEntry::new("github")]),
        ecosystems: vec![],
        current_branch: None,
    });
    let conflicts = engine.conflicts();
    assert!(
        conflicts
            .iter()
            .any(|c| c.kind == ConflictKind::NearDuplicate
                && c.candidates.iter().any(|(t, _)| t == "GitHub")),
        "{conflicts:?}"
    );
}

#[test]
fn hint_prompt_orders_by_relevance_and_respects_length() {
    let mut engine = base_engine();
    engine.add_personal_term(TermEntry::new("Kubrick"));
    engine.set_project(ProjectLexicon {
        lexicon: Lexicon::from_entries(
            Layer::Project,
            [TermEntry::new("order-service").weight(3.0)],
        ),
        ecosystems: vec!["rust".into()],
        current_branch: None,
    });
    let hint = engine.hint_prompt(200);
    assert!(hint.chars().count() <= 200);
    assert!(hint.starts_with("Kubrick, order-service"), "{hint}");
    // Rust-tagged base terms come before untagged ones of similar weight.
    let rust_pos = hint.find("Cargo").or_else(|| hint.find("Tokio"));
    assert!(rust_pos.is_some(), "{hint}");
    let phrases = engine.hint_phrases(10);
    assert_eq!(phrases.len(), 10);
    assert_eq!(phrases[0], "Kubrick");
    assert!(engine.hint_prompt(3).chars().count() <= 3);
    assert_eq!(engine.hint_prompt(0), "");
}

#[test]
fn learning_from_edits_promotes_and_corrects() {
    let mut engine = small_engine();
    let shown = "deploy the order service with tilt";
    let edited = "deploy the order-service with Tilt";
    let first = engine.learn_from_edit(shown, edited);
    assert_eq!(first.len(), 2);
    assert!(first.iter().all(|m| !m.promoted));
    assert_eq!(engine.correct(shown).text, shown);
    let second = engine.learn_from_edit(shown, edited);
    assert!(second.iter().all(|m| m.promoted));
    assert_eq!(engine.correct(shown).text, edited);
    assert!(engine.personal().lexicon().get("order-service").is_some());
}

#[test]
fn reverting_a_correction_suppresses_it() {
    let mut engine = small_engine();
    // The engine turned "cloud code" into "Claude Code"; the user means a
    // folder literally named cloud code and reverts it twice.
    let corrected = engine.correct("open cloud code folder").text;
    assert_eq!(corrected, "open Claude Code folder");
    for _ in 0..2 {
        let learned = engine.learn_from_edit(&corrected, "open cloud code folder");
        assert!(learned[0].rejection);
    }
    assert_eq!(
        engine.correct("open cloud code folder").text,
        "open cloud code folder"
    );
    assert_eq!(engine.personal().suppressions().len(), 1);
}

#[test]
fn real_lexicon_corrections() {
    let engine = base_engine();
    let cases = [
        (
            "ask cloud code to run cube control get pods",
            "ask Claude Code to run kubectl get pods",
        ),
        (
            "open a pull request on git hub",
            "open a pull request on GitHub",
        ),
        ("install it with p n p m", "install it with pnpm"),
        (
            "start the next js dev server",
            "start the Next.js dev server",
        ),
        (
            "クロードコードでテストを実行して",
            "Claude Codeでテストを実行して",
        ),
        (
            "쿠버네티스 클러스터를 확인해줘",
            "Kubernetes 클러스터를 확인해줘",
        ),
        (
            "abre un pull request en guit jab",
            "abre un pull request en GitHub",
        ),
        ("用道克部署到库伯内蒂斯", "用Docker部署到Kubernetes"),
        (
            "frag klaude code nach dem pull request",
            "frag Claude Code nach dem pull request",
        ),
        ("demande à chat j'ai pété", "demande à ChatGPT"),
    ];
    for (input, expected) in cases {
        assert_eq!(engine.correct(input).text, expected, "input: {input}");
    }
    // Plain prose stays as is.
    for s in [
        "make the login page load faster and add a test for it",
        "go home and rest",
        "let me know what you think about this design",
        "read me the last output",
    ] {
        let r = engine.correct(s);
        assert!(r.corrections.is_empty(), "{s:?} -> {:?}", r.corrections);
    }
}
