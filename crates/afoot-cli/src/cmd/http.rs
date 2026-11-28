//! The one HTTP client the CLI's own network commands share (GitHub
//! releases for `afoot self update`, PyPI for `afoot python upgrade`).

use anyhow::{Context, Result};

/// A blocking client with a bounded timeout and an identifying user agent.
pub fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(60))
        .user_agent(concat!("agentic-footprint/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// `GET url` (with a bearer `token` when given), the whole body as bytes.
/// A non-2xx answer is an error carrying its status and body; 401/403/429
/// also name `--token`, the usual remedy for GitHub's rate limit.
pub fn get(agent: &ureq::Agent, url: &str, token: Option<&str>) -> Result<Vec<u8>> {
    let mut request = agent.get(url);
    if let Some(token) = token {
        request = request.set("Authorization", &format!("Bearer {token}"));
    }
    let response = match request.call() {
        Ok(response) => response,
        Err(ureq::Error::Status(code, response)) => {
            let body = response.into_string().unwrap_or_default();
            let hint = if matches!(code, 401 | 403 | 429) {
                " (rate limited or unauthorized? pass --token or set GITHUB_TOKEN)"
            } else {
                ""
            };
            anyhow::bail!("GET {url} answered {code}{hint}: {}", body.trim())
        }
        Err(error) => return Err(error).with_context(|| format!("GET {url}")),
    };
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut response.into_reader(), &mut bytes)
        .with_context(|| format!("download {url}"))?;
    Ok(bytes)
}

/// [`get`], parsed as JSON.
pub fn get_json(agent: &ureq::Agent, url: &str, token: Option<&str>) -> Result<serde_json::Value> {
    serde_json::from_slice(&get(agent, url, token)?).with_context(|| format!("parse {url}"))
}
