use std::time::{Duration, Instant};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;
use regex::Regex;
use reqwest::{
    Client, StatusCode, Url,
    header::{LOCATION, USER_AGENT},
    redirect::Policy,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::{AnyError, any_error};

const CLIENT_ID: &str = "student-personal-cabinet";
const REDIRECT_URI: &str = "https://my.itmo.ru/login/callback";
const PROVIDER: &str = "https://id.itmo.ru/auth/realms/itmo";

struct CachedToken {
    value: String,
    valid_until: Instant,
}

enum AuthorizationResponse {
    LoginPage(reqwest::Response),
    Callback(Url),
}

pub struct AuthClient {
    http: Client,
    username: String,
    password: String,
    token: Mutex<Option<CachedToken>>,
}

impl AuthClient {
    pub fn new(username: String, password: String) -> Result<Self, AnyError> {
        let http = Client::builder()
            .cookie_store(true)
            .redirect(Policy::none())
            .timeout(Duration::from_secs(30))
            .user_agent("itmo-calendar-sync/0.1")
            .build()?;

        Ok(Self {
            http,
            username,
            password,
            token: Mutex::new(None),
        })
    }

    pub async fn access_token(&self) -> Result<String, AnyError> {
        {
            let token = self.token.lock().await;
            if let Some(token) = token
                .as_ref()
                .filter(|token| token.valid_until > Instant::now())
            {
                return Ok(token.value.clone());
            }
        }

        let fresh = self.authenticate().await?;
        let value = fresh.access_token;
        let safety_margin = fresh.expires_in.min(60);
        let lifetime = fresh.expires_in.saturating_sub(safety_margin).max(1);

        *self.token.lock().await = Some(CachedToken {
            value: value.clone(),
            valid_until: Instant::now() + Duration::from_secs(lifetime),
        });

        Ok(value)
    }

    pub async fn invalidate(&self) {
        *self.token.lock().await = None;
    }

    async fn authenticate(&self) -> Result<TokenResponse, AnyError> {
        let mut verifier_bytes = [0_u8; 48];
        rand::rng().fill_bytes(&mut verifier_bytes);
        let verifier = URL_SAFE_NO_PAD.encode(verifier_bytes);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));

        let mut state_bytes = [0_u8; 24];
        rand::rng().fill_bytes(&mut state_bytes);
        let state = URL_SAFE_NO_PAD.encode(state_bytes);

        let authorization_url = format!("{PROVIDER}/protocol/openid-connect/auth");
        let authorization = self
            .follow_authorization_redirects(self.http.get(authorization_url).query(&[
                ("protocol", "oauth2".to_owned()),
                ("response_type", "code".to_owned()),
                ("client_id", CLIENT_ID.to_owned()),
                ("redirect_uri", REDIRECT_URI.to_owned()),
                ("scope", "openid".to_owned()),
                ("state", state.clone()),
                ("code_challenge_method", "S256".to_owned()),
                ("code_challenge", challenge),
            ]))
            .await?;

        let redirect = match authorization {
            AuthorizationResponse::Callback(redirect) => redirect,
            AuthorizationResponse::LoginPage(response) => {
                if !response.status().is_success() {
                    return Err(any_error(format!(
                        "ITMO authorization page returned {}",
                        response.status()
                    )));
                }

                let page = response.text().await?;
                let login_action = extract_login_action(&page)?;

                let login_response = self
                    .http
                    .post(login_action)
                    .header(USER_AGENT, "itmo-calendar-sync/0.1")
                    .form(&[
                        ("username", self.username.as_str()),
                        ("password", self.password.as_str()),
                        ("credentialId", ""),
                    ])
                    .send()
                    .await?;

                if !matches!(
                    login_response.status(),
                    StatusCode::FOUND | StatusCode::SEE_OTHER | StatusCode::TEMPORARY_REDIRECT
                ) {
                    return Err(any_error(
                        "ITMO login was rejected; check the username, password, or required login action",
                    ));
                }

                redirect_url(&login_response)?
            }
        };

        if !is_login_callback(&redirect) {
            return Err(any_error("ITMO login returned an unexpected redirect"));
        }

        let code = extract_authorization_code(&redirect, &state)?;
        let token_url = format!("{PROVIDER}/protocol/openid-connect/token");
        let token_response = self
            .http
            .post(token_url)
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", CLIENT_ID),
                ("redirect_uri", REDIRECT_URI),
                ("code", code.as_str()),
                ("code_verifier", verifier.as_str()),
            ])
            .send()
            .await?;

        if !token_response.status().is_success() {
            return Err(any_error(format!(
                "ITMO token endpoint returned {}",
                token_response.status()
            )));
        }

        Ok(token_response.json::<TokenResponse>().await?)
    }

    async fn follow_authorization_redirects(
        &self,
        mut request: reqwest::RequestBuilder,
    ) -> Result<AuthorizationResponse, AnyError> {
        for _ in 0..10 {
            let response = request.send().await?;
            if !response.status().is_redirection() {
                return Ok(AuthorizationResponse::LoginPage(response));
            }

            let next_url = redirect_url(&response)?;
            if is_login_callback(&next_url) {
                return Ok(AuthorizationResponse::Callback(next_url));
            }

            request = self.http.get(next_url);
        }

        Err(any_error("too many redirects during ITMO authorization"))
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

fn redirect_url(response: &reqwest::Response) -> Result<Url, AnyError> {
    let location = response
        .headers()
        .get(LOCATION)
        .ok_or_else(|| any_error("ITMO redirect has no location"))?
        .to_str()
        .map_err(|_| any_error("ITMO redirect location is not valid text"))?;

    Url::parse(location)
        .or_else(|_| response.url().join(location))
        .map_err(|error| any_error(format!("invalid ITMO redirect: {error}")))
}

fn is_login_callback(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("my.itmo.ru")
        && url.port_or_known_default() == Some(443)
        && url.path() == "/login/callback"
}

fn extract_authorization_code(redirect: &Url, expected_state: &str) -> Result<String, AnyError> {
    let mut code = None;
    let mut returned_state = None;
    for (key, value) in redirect.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => returned_state = Some(value.into_owned()),
            _ => {}
        }
    }

    if returned_state.as_deref() != Some(expected_state) {
        return Err(any_error("ITMO login returned an invalid OAuth state"));
    }

    code.ok_or_else(|| any_error("ITMO login returned no authorization code"))
}

fn extract_login_action(page: &str) -> Result<String, AnyError> {
    let pattern = Regex::new(r#""loginAction"\s*:\s*"([^"]+)""#)
        .map_err(|error| any_error(format!("cannot prepare login parser: {error}")))?;
    let captured = pattern
        .captures(page)
        .and_then(|captures| captures.get(1))
        .ok_or_else(|| any_error("ITMO login form was not found"))?
        .as_str();

    let quoted = format!("\"{captured}\"");
    let decoded = serde_json::from_str::<String>(&quoted)
        .map_err(|_| any_error("ITMO login form address could not be decoded"))?;

    Ok(decoded.replace("&amp;", "&"))
}

#[cfg(test)]
mod tests {
    use super::{extract_authorization_code, extract_login_action, is_login_callback};
    use reqwest::Url;

    #[test]
    fn extracts_login_action_from_current_page_format() {
        let page = r#"{
            "url": {
                "loginAction": "https:\/\/id.itmo.ru\/auth\/realms\/itmo\/login-actions\/authenticate?session_code=abc&amp;execution=def"
            }
        }"#;

        let action = extract_login_action(page).expect("login action must be parsed");

        assert_eq!(
            action,
            "https://id.itmo.ru/auth/realms/itmo/login-actions/authenticate?session_code=abc&execution=def"
        );
    }

    #[test]
    fn accepts_direct_sso_callback() {
        let callback = Url::parse(
            "https://my.itmo.ru/login/callback?session_state=session&state=expected&code=auth-code",
        )
        .expect("callback URL must be valid");

        assert!(is_login_callback(&callback));
        assert_eq!(
            extract_authorization_code(&callback, "expected")
                .expect("authorization code must be parsed"),
            "auth-code"
        );
    }

    #[test]
    fn rejects_callback_with_wrong_state() {
        let callback = Url::parse("https://my.itmo.ru/login/callback?state=wrong&code=auth-code")
            .expect("callback URL must be valid");

        let error = extract_authorization_code(&callback, "expected")
            .expect_err("wrong state must be rejected");

        assert_eq!(
            error.to_string(),
            "ITMO login returned an invalid OAuth state"
        );
    }

    #[test]
    fn rejects_lookalike_callback_host() {
        let callback =
            Url::parse("https://my.itmo.ru.attacker.example/login/callback?state=x&code=y")
                .expect("callback URL must be valid");

        assert!(!is_login_callback(&callback));
    }

    #[test]
    fn rejects_wrong_callback_path() {
        let callback = Url::parse("https://my.itmo.ru/login/callback/extra?state=x&code=y")
            .expect("callback URL must be valid");

        assert!(!is_login_callback(&callback));
    }
}
