//! Repository reference parsing shared by adapters.

/// A GitHub repository reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubRepo {
    pub owner: String,
    pub name: String,
}

impl GithubRepo {
    /// Accepts `owner/repo`, `github.com/owner/repo` and
    /// `https://github.com/owner/repo(.git)`.
    pub fn parse(input: &str) -> Option<Self> {
        let s = input.trim().trim_end_matches('/');
        let s = s
            .strip_prefix("https://")
            .or_else(|| s.strip_prefix("http://"))
            .unwrap_or(s);
        let s = s.strip_prefix("www.").unwrap_or(s);
        let s = s.strip_prefix("github.com/").unwrap_or(s);
        let mut parts = s.split('/');
        let owner = parts.next()?.trim();
        let name = parts.next()?.trim().trim_end_matches(".git");
        if parts.next().is_some() || owner.is_empty() || name.is_empty() {
            return None;
        }
        let valid = |p: &str| {
            p.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        };
        if !valid(owner) || !valid(name) {
            return None;
        }
        Some(Self {
            owner: owner.to_owned(),
            name: name.to_owned(),
        })
    }

    pub fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }

    pub fn https_url(&self) -> String {
        format!("https://github.com/{}/{}", self.owner, self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_forms() {
        for s in [
            "octo/hello",
            "github.com/octo/hello",
            "https://github.com/octo/hello",
            "https://github.com/octo/hello.git",
            "https://github.com/octo/hello/",
        ] {
            assert_eq!(
                GithubRepo::parse(s),
                Some(GithubRepo {
                    owner: "octo".into(),
                    name: "hello".into()
                }),
                "{s}"
            );
        }
        assert_eq!(GithubRepo::parse("octo"), None);
        assert_eq!(GithubRepo::parse("a/b/c"), None);
        assert_eq!(GithubRepo::parse("a b/c"), None);
    }
}
