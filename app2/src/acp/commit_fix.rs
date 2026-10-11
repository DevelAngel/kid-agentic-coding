use std::path::PathBuf;

#[derive(Debug, PartialEq, Eq)]
pub struct CommitFixRequest {
    pub tldr: String,
    pub why: String,
    pub what: String,
    pub amend: bool,
    pub cwd: Option<PathBuf>,
}

impl CommitFixRequest {
    pub fn matches_tool_title(title: &str) -> bool {
        title.eq_ignore_ascii_case("git_commit_with_fix")
            || title.eq_ignore_ascii_case("git commit with fix")
    }

    pub fn prompt(&self) -> String {
        format!(
            "Commit the current changes. Compose a correctly formatted commit message; do not copy the analysis fields verbatim. Amend the previous commit when amend is true; otherwise create a new commit. Run checks, lint, and tests if the commit fails or additional verification is needed.\n\nTL;DR: {}\nWhy: {}\nWhat: {}\nAmend previous commit: {}\nWorking directory: {}",
            self.tldr,
            self.why,
            self.what,
            self.amend,
            self.cwd.as_deref().map_or_else(
                || "current directory".to_owned(),
                |path| path.display().to_string()
            )
        )
    }
}

impl TryFrom<&serde_json::Value> for CommitFixRequest {
    type Error = &'static str;

    fn try_from(value: &serde_json::Value) -> Result<Self, Self::Error> {
        let object = value.as_object().ok_or("tool input must be an object")?;
        let tldr = object
            .get("tldr")
            .and_then(serde_json::Value::as_str)
            .ok_or("missing string field: tldr")?
            .to_owned();
        let why = object
            .get("why")
            .and_then(serde_json::Value::as_str)
            .ok_or("missing string field: why")?
            .to_owned();
        let what = object
            .get("what")
            .and_then(serde_json::Value::as_str)
            .ok_or("missing string field: what")?
            .to_owned();
        let amend = object
            .get("amend")
            .and_then(serde_json::Value::as_bool)
            .ok_or("missing boolean field: amend")?;
        let cwd = match object.get("cwd") {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(path)) => Some(PathBuf::from(path)),
            Some(_) => return Err("field cwd must be a string or null"),
        };

        Ok(Self {
            tldr,
            why,
            what,
            amend,
            cwd,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_commit_fix_arguments() {
        let input = serde_json::json!({
            "tldr": "Fix prompt handoff",
            "why": "The transition loses context",
            "what": "Carry typed request data",
            "amend": true,
            "cwd": "/workspace/project"
        });

        let request = CommitFixRequest::try_from(&input).unwrap();

        assert_eq!(
            request,
            CommitFixRequest {
                tldr: "Fix prompt handoff".to_owned(),
                why: "The transition loses context".to_owned(),
                what: "Carry typed request data".to_owned(),
                amend: true,
                cwd: Some(PathBuf::from("/workspace/project")),
            }
        );
    }

    #[test]
    fn rejects_incomplete_commit_fix_arguments() {
        let input = serde_json::json!({
            "tldr": "Fix prompt handoff",
            "why": "The transition loses context",
            "amend": false
        });

        assert_eq!(
            CommitFixRequest::try_from(&input),
            Err("missing string field: what")
        );
    }

    #[test]
    fn prompt_preserves_commit_fix_context() {
        let request = CommitFixRequest {
            tldr: "Fix handoff".to_owned(),
            why: "The context was lost".to_owned(),
            what: "Pass typed parameters".to_owned(),
            amend: true,
            cwd: Some(PathBuf::from("/workspace/project")),
        };

        let prompt = request.prompt();

        assert!(prompt.contains("TL;DR: Fix handoff"));
        assert!(prompt.contains("Why: The context was lost"));
        assert!(prompt.contains("What: Pass typed parameters"));
        assert!(prompt.contains("Amend previous commit: true"));
        assert!(prompt.contains("Working directory: /workspace/project"));
    }

    #[test]
    fn accepts_missing_optional_working_directory() {
        let input = serde_json::json!({
            "tldr": "Fix prompt handoff",
            "why": "The transition loses context",
            "what": "Carry typed request data",
            "amend": false
        });

        let request = CommitFixRequest::try_from(&input).unwrap();

        assert_eq!(request.cwd, None);
    }
}
