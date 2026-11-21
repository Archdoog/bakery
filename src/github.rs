//! GitHub Actions runner registration/removal tokens for an org.

use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;

const API: &str = "https://api.github.com";

#[derive(Debug, Error)]
pub enum GithubError {
    #[error("GitHub API {status} on {method} {url}: {body}")]
    Api {
        method: String,
        url: String,
        status: u16,
        body: String,
    },
    #[error("GitHub API transport error on {method} {url}: {message}")]
    Transport {
        method: String,
        url: String,
        message: String,
    },
    #[error("decoding GitHub {method} {url} response: {message}")]
    Decode {
        method: String,
        url: String,
        message: String,
    },
}

pub type Result<T> = std::result::Result<T, GithubError>;

#[derive(Debug, Clone, Deserialize)]
pub struct Runner {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub status: String,
}

#[derive(Debug, Deserialize)]
struct TokenResp {
    token: String,
}

#[derive(Debug, Deserialize)]
struct RunnersList {
    #[serde(default)]
    runners: Vec<Runner>,
}

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
}

/// Resolve `version` to a release number, using `pat` for GitHub authentication.
///
/// # Errors
/// Return an error if GitHub cannot supply the latest release.
pub(crate) fn resolve_runner_version(version: &str, pat: &str) -> Result<String> {
    if version != "latest" {
        return Ok(version.to_string());
    }
    latest_runner_version(&format!("{API}/repos/actions/runner/releases/latest"), pat)
}

fn latest_runner_version(url: &str, pat: &str) -> Result<String> {
    let release: Release = call_json(ureq::get(url), pat)?;
    Ok(release.tag_name.trim_start_matches('v').to_string())
}

pub fn registration_token(org: &str, pat: &str) -> Result<String> {
    let url = format!("{API}/orgs/{org}/actions/runners/registration-token");
    let resp: TokenResp = call_json(ureq::post(&url), pat)?;
    Ok(resp.token)
}

pub fn removal_token(org: &str, pat: &str) -> Result<String> {
    let url = format!("{API}/orgs/{org}/actions/runners/remove-token");
    let resp: TokenResp = call_json(ureq::post(&url), pat)?;
    Ok(resp.token)
}

pub fn list_runners(org: &str, pat: &str) -> Result<Vec<Runner>> {
    // Orgs with 100+ self-hosted runners would need pagination, which isn't us.
    let url = format!("{API}/orgs/{org}/actions/runners?per_page=100");
    let resp: RunnersList = call_json(ureq::get(&url), pat)?;
    Ok(resp.runners)
}

pub fn find_runner(org: &str, pat: &str, name: &str) -> Result<Option<Runner>> {
    Ok(list_runners(org, pat)?.into_iter().find(|r| r.name == name))
}

pub fn delete_runner(org: &str, pat: &str, runner_id: u64) -> Result<()> {
    let url = format!("{API}/orgs/{org}/actions/runners/{runner_id}");
    let (method, url_s) = ("DELETE".to_string(), url.clone());
    auth(ureq::delete(&url), pat)
        .call()
        .map(|_| ())
        .map_err(|e| into_error(&method, &url_s, e))
}

fn call_json<T: serde::de::DeserializeOwned>(req: ureq::Request, pat: &str) -> Result<T> {
    let method = req.method().to_string();
    let url = req.url().to_string();
    let resp = auth(req, pat)
        .call()
        .map_err(|e| into_error(&method, &url, e))?;
    resp.into_json::<T>().map_err(|e| GithubError::Decode {
        method,
        url,
        message: e.to_string(),
    })
}

fn auth(req: ureq::Request, pat: &str) -> ureq::Request {
    req.set("Authorization", &format!("Bearer {pat}"))
        .set("Accept", "application/vnd.github+json")
        .set("X-GitHub-Api-Version", "2022-11-28")
        .set("User-Agent", "bakery")
        .timeout(Duration::from_secs(30))
}

fn into_error(method: &str, url: &str, e: ureq::Error) -> GithubError {
    match e {
        ureq::Error::Status(status, resp) => GithubError::Api {
            method: method.into(),
            url: url.into(),
            status,
            body: resp.into_string().unwrap_or_default().trim().to_string(),
        },
        ureq::Error::Transport(t) => GithubError::Transport {
            method: method.into(),
            url: url.into(),
            message: t.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn release_response(status: &str, body: &str) -> (String, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/releases/latest", listener.local_addr().unwrap());
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            while !request.ends_with(b"\r\n\r\n") {
                let count = stream.read(&mut buffer).unwrap();
                assert_ne!(count, 0);
                request.extend_from_slice(&buffer[..count]);
            }
            stream.write_all(response.as_bytes()).unwrap();
            String::from_utf8(request).unwrap()
        });
        (url, server)
    }

    #[test]
    fn latest_release_supplies_the_download_version() {
        let (url, server) = release_response("200 OK", r#"{"tag_name":"v2.338.2"}"#);
        assert_eq!(
            latest_runner_version(&url, "test-token").unwrap(),
            "2.338.2"
        );
        let request = server.join().unwrap().to_lowercase();
        assert!(request.starts_with("get /releases/latest http/1.1\r\n"));
        assert!(request.contains("authorization: bearer test-token\r\n"));
    }

    #[test]
    fn unavailable_latest_release_returns_an_error() {
        let (url, server) =
            release_response("503 Service Unavailable", r#"{"message":"unavailable"}"#);
        assert!(matches!(
            latest_runner_version(&url, "test-token"),
            Err(GithubError::Api { status: 503, .. })
        ));
        server.join().unwrap();
    }

    #[test]
    fn pinned_versions_need_no_release_lookup() {
        assert_eq!(resolve_runner_version("2.337.0", "").unwrap(), "2.337.0");
    }
}
