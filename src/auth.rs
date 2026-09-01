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
        let response = self
            .get_following_redirects(self.http.get(authorization_url).query(&[
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

        let location = login_response
            .headers()
            .get(LOCATION)
            .ok_or_else(|| any_error("ITMO login response has no redirect location"))?
            .to_str()
            .map_err(|_| any_error("ITMO login redirect is not valid text"))?;

        let redirect = Url::parse(location)
            .or_else(|_| login_response.url().join(location))
            .map_err(|error| any_error(format!("invalid ITMO login redirect: {error}")))?;

        let mut code = None;
        let mut returned_state = None;
        for (key, value) in redirect.query_pairs() {
            match key.as_ref() {
                "code" => code = Some(value.into_owned()),
                "state" => returned_state = Some(value.into_owned()),
                _ => {}
            }
        }

        if returned_state.as_deref() != Some(state.as_str()) {
            return Err(any_error("ITMO login returned an invalid OAuth state"));
        }

        let code = code.ok_or_else(|| any_error("ITMO login returned no authorization code"))?;
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

    async fn get_following_redirects(
        &self,
        mut request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, AnyError> {
        for _ in 0..10 {
            let response = request.send().await?;
            if !response.status().is_redirection() {
                return Ok(response);
            }

            let location = response
                .headers()
                .get(LOCATION)
                .ok_or_else(|| any_error("ITMO redirect has no location"))?
                .to_str()
                .map_err(|_| any_error("ITMO redirect location is not valid text"))?;
            let next_url = response
                .url()
                .join(location)
                .map_err(|error| any_error(format!("invalid ITMO redirect: {error}")))?;
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
