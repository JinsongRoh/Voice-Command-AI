//! Lightweight adapters for agents without a deeper integration (yet).
//!
//! They discover sessions by process name, build launch specs and rely on
//! [`crate::heuristics`] for state. Program names and markers below follow
//! each project's published install instructions; none of these agents is
//! installed on the development machine, so all of them are marked
//! unverified in [`Capabilities::verified`].

use async_trait::async_trait;
use vca_core::{AgentKind, LaunchRequest};

use crate::adapter::{AgentAdapter, basic_launch_spec};
use crate::error::Result;
use crate::process::{ProcessMatcher, ProcessTable};
use crate::types::{Capabilities, DiscoveredSession, LaunchSpec, Profile};

/// Static description of an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSpec {
    pub kind: AgentKind,
    pub display_name: String,
    /// Executable used to launch the agent.
    pub program: String,
    pub process_names: Vec<String>,
    /// Script path markers when the agent runs under node or python.
    pub script_markers: Vec<String>,
    /// Environment variable that relocates the agent's configuration, for
    /// profile isolation, if the agent has one.
    pub profile_env_var: Option<String>,
    pub capabilities: Capabilities,
}

impl AgentSpec {
    fn new(
        kind: AgentKind,
        display_name: &str,
        program: &str,
        names: &[&str],
        markers: &[&str],
    ) -> Self {
        Self {
            kind,
            display_name: display_name.into(),
            program: program.into(),
            process_names: names.iter().map(|s| (*s).to_owned()).collect(),
            script_markers: markers.iter().map(|s| (*s).to_owned()).collect(),
            profile_env_var: None,
            capabilities: Capabilities {
                output_heuristics: true,
                ..Capabilities::default()
            },
        }
    }

    /// A spec for any command, identified by its executable name.
    pub fn generic(name: &str) -> Self {
        Self::new(
            AgentKind::Generic(name.to_owned()),
            name,
            name,
            &[name],
            &[],
        )
    }

    /// Specs of the agents VCA knows by name (Claude Code and Codex have
    /// dedicated adapters and are not included).
    pub fn builtin() -> Vec<Self> {
        vec![
            Self::new(
                AgentKind::GeminiCli,
                "Gemini CLI",
                "gemini",
                &["gemini"],
                &["@google/gemini-cli"],
            ),
            Self::new(
                AgentKind::Antigravity,
                "Google Antigravity",
                "antigravity",
                &["antigravity"],
                &[],
            ),
            Self::new(
                AgentKind::Grok,
                "Grok CLI",
                "grok",
                &["grok"],
                &["grok-cli"],
            ),
            Self::new(
                AgentKind::CursorCli,
                "Cursor CLI",
                "cursor-agent",
                &["cursor-agent"],
                &["cursor-agent"],
            ),
            Self::new(
                AgentKind::CopilotCli,
                "GitHub Copilot CLI",
                "copilot",
                &["copilot"],
                &["@github/copilot"],
            ),
            Self::new(AgentKind::Aider, "Aider", "aider", &["aider"], &["aider"]),
            Self::new(
                AgentKind::Opencode,
                "opencode",
                "opencode",
                &["opencode"],
                &["opencode-ai"],
            ),
            Self::new(
                AgentKind::QwenCode,
                "Qwen Code",
                "qwen",
                &["qwen"],
                &["@qwen-code/qwen-code"],
            ),
        ]
    }
}

/// Adapter driven by an [`AgentSpec`].
#[derive(Debug, Clone)]
pub struct GenericAdapter {
    spec: AgentSpec,
}

impl GenericAdapter {
    pub fn new(spec: AgentSpec) -> Self {
        Self { spec }
    }

    pub fn spec(&self) -> &AgentSpec {
        &self.spec
    }
}

#[async_trait]
impl AgentAdapter for GenericAdapter {
    fn kind(&self) -> AgentKind {
        self.spec.kind.clone()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            profile_isolation: self.spec.profile_env_var.is_some(),
            ..self.spec.capabilities
        }
    }

    fn process_matcher(&self) -> ProcessMatcher {
        let names: Vec<&str> = self.spec.process_names.iter().map(String::as_str).collect();
        let markers: Vec<&str> = self
            .spec
            .script_markers
            .iter()
            .map(String::as_str)
            .collect();
        ProcessMatcher::new(&names, &markers)
    }

    async fn discover(&self, procs: &ProcessTable) -> Result<Vec<DiscoveredSession>> {
        Ok(procs
            .find(&self.process_matcher())
            .into_iter()
            .map(|p| DiscoveredSession {
                agent: self.spec.kind.clone(),
                agent_session_id: None,
                project_dir: p.cwd.clone(),
                pid: Some(p.pid),
                last_activity: Some(p.start_time * 1000),
                title: None,
                transcript_path: None,
            })
            .collect())
    }

    fn launch_spec(&self, req: &LaunchRequest, profile: Option<&Profile>) -> Result<LaunchSpec> {
        let env = profile.map(|p| self.profile_env(p)).unwrap_or_default();
        basic_launch_spec(&self.spec.program, req, env)
    }

    fn profile_env(&self, profile: &Profile) -> Vec<(String, String)> {
        match &self.spec.profile_env_var {
            Some(var) => vec![(
                var.clone(),
                profile.config_dir.to_string_lossy().into_owned(),
            )],
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::process::ProcInfo;

    #[test]
    fn builtin_covers_every_named_agent() {
        let kinds: Vec<AgentKind> = AgentSpec::builtin().into_iter().map(|s| s.kind).collect();
        for k in [
            AgentKind::GeminiCli,
            AgentKind::Antigravity,
            AgentKind::Grok,
            AgentKind::CursorCli,
            AgentKind::CopilotCli,
            AgentKind::Aider,
            AgentKind::Opencode,
            AgentKind::QwenCode,
        ] {
            assert!(kinds.contains(&k), "{k:?} missing");
        }
        assert!(
            AgentSpec::builtin()
                .iter()
                .all(|s| !s.capabilities.verified)
        );
    }

    #[tokio::test]
    async fn discovers_by_process_and_launches() {
        let procs = ProcessTable::from_procs(vec![ProcInfo {
            pid: 4242,
            parent: None,
            name: "node".into(),
            cmd: vec![
                "node".into(),
                "/usr/lib/node_modules/@qwen-code/qwen-code/cli.js".into(),
            ],
            exe: None,
            cwd: Some(PathBuf::from("/src/app")),
            start_time: 10,
        }]);
        let qwen = crate::adapter::adapter_for(&AgentKind::QwenCode);
        let found = qwen.discover(&procs).await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].pid, Some(4242));
        assert_eq!(found[0].project_dir, Some(PathBuf::from("/src/app")));

        let generic = crate::adapter::adapter_for(&AgentKind::Generic("kubectl".into()));
        let req = LaunchRequest {
            agent: AgentKind::Generic("kubectl".into()),
            project_dir: "/src/app".into(),
            create_dir: false,
            profile: None,
            args: vec!["get".into(), "pods".into()],
            display_name: None,
        };
        let spec = generic.launch_spec(&req, None).unwrap();
        assert_eq!((spec.program.as_str(), spec.args.len()), ("kubectl", 2));
        assert!(generic.profile_env(&Profile::new("a", "/b")).is_empty());
    }
}
