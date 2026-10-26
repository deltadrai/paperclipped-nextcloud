//! A small OCS / JSON client for Nextcloud.

use anyhow::{Context, anyhow};
use reqwest::{Method, StatusCode};
use serde_json::Value;

/// Percent-encodes one URL path segment (user ids, group names, tokens).
pub fn seg(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[derive(Clone)]
pub struct Client {
    base: String,
    http: reqwest::Client,
}

/// An OCS response: the HTTP status, `ocs.meta.statuscode` and `ocs.data`.
#[derive(Debug)]
pub struct Ocs {
    pub http: StatusCode,
    pub code: i64,
    pub message: String,
    pub data: Value,
}

impl Ocs {
    /// OCS v2 mirrors the HTTP status (200, 201); v1-style routes report 100.
    pub fn ok(&self) -> bool {
        self.http.is_success() && (self.code == 100 || (200..300).contains(&self.code))
    }

    pub fn into_result(self, what: &str) -> anyhow::Result<Value> {
        if self.ok() {
            Ok(self.data)
        } else {
            Err(anyhow!(
                "{what}: HTTP {} / OCS {} {}",
                self.http.as_u16(),
                self.code,
                self.message
            ))
        }
    }
}

pub struct Auth<'a> {
    pub user: &'a str,
    pub password: &'a str,
}

impl Client {
    pub fn new(base: &str) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!(
                "paperclipped-nextcloud/",
                env!("CARGO_PKG_VERSION")
            ))
            .timeout(std::time::Duration::from_secs(60))
            .build()?;
        Ok(Self {
            base: base.trim_end_matches('/').to_string(),
            http,
        })
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn request(&self, method: Method, path: &str, auth: &Auth<'_>) -> reqwest::RequestBuilder {
        self.http
            .request(method, self.url(path))
            .basic_auth(auth.user, Some(auth.password))
            .header("OCS-APIRequest", "true")
            .header("Accept", "application/json")
    }

    /// An OCS call with an optional form body. `user_agent` overrides the client's (Nextcloud
    /// names app passwords after the user agent that requested them).
    pub async fn ocs(
        &self,
        method: Method,
        path: &str,
        auth: &Auth<'_>,
        form: &[(&str, &str)],
        user_agent: Option<&str>,
    ) -> anyhow::Result<Ocs> {
        let mut req = self.request(method.clone(), path, auth);
        if !form.is_empty() {
            req = req.form(form);
        }
        if let Some(ua) = user_agent {
            req = req.header("User-Agent", ua);
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("{method} {path}"))?;
        let http = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let v: Value = match serde_json::from_str(&body) {
            Ok(v) => v,
            // 401 and some proxies answer without an OCS envelope.
            Err(_) => {
                return Ok(Ocs {
                    http,
                    code: http.as_u16() as i64,
                    message: snippet(&body),
                    data: Value::Null,
                });
            }
        };
        let meta = &v["ocs"]["meta"];
        Ok(Ocs {
            http,
            code: meta["statuscode"].as_i64().unwrap_or(http.as_u16() as i64),
            message: meta["message"].as_str().unwrap_or_default().to_string(),
            data: v["ocs"]["data"].clone(),
        })
    }

    /// A JSON call to an app controller (Nextcloud Mail). Returns status and parsed body.
    pub async fn json(
        &self,
        method: Method,
        path: &str,
        auth: &Auth<'_>,
        body: Option<&Value>,
    ) -> anyhow::Result<(StatusCode, Value)> {
        let mut req = self.request(method.clone(), path, auth);
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("{method} {path}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let value = serde_json::from_str(&text).unwrap_or(Value::String(snippet(&text)));
        Ok((status, value))
    }

    /// A raw WebDAV call (MKCOL, PROPFIND). Returns the status only.
    pub async fn dav(
        &self,
        method: &str,
        path: &str,
        auth: &Auth<'_>,
    ) -> anyhow::Result<StatusCode> {
        let method = Method::from_bytes(method.as_bytes())?;
        let resp = self
            .http
            .request(method.clone(), self.url(path))
            .basic_auth(auth.user, Some(auth.password))
            .header("Depth", "0")
            .send()
            .await
            .with_context(|| format!("{method} {path}"))?;
        Ok(resp.status())
    }
}

fn snippet(s: &str) -> String {
    s.chars().take(200).collect()
}
