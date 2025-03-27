use crate::{
    SUPER_CLIENT, TITLE,
    api::{AppState, InnerState},
    stream::{ClewdrConfig, ClewdrTransformer},
    utils::{ClewdrError, ENDPOINT, TEST_MESSAGE, check_res_err, header_ref},
};
use axum::{
    Json,
    body::Body,
    extract::{Request, State},
    http::HeaderMap,
};
use bytes::Bytes;
use futures::pin_mut;
use regex::{Regex, RegexBuilder};
use rquest::header::{COOKIE, ORIGIN, REFERER};
use serde::{de, ser};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_stream::{Stream, StreamExt, wrappers::ReceiverStream};
use tracing::info;

pub async fn stream_example(
    State(state): State<AppState>,
    header: HeaderMap,
    Json(payload): Json<Value>,
) -> Body {
    // Create a channel for streaming response chunks to the client
    let (tx, rx) = mpsc::channel::<Result<Bytes, axum::Error>>(32);

    // Configure the transformer
    let config = ClewdrConfig::new("xx", "pro", true, 8, true);
    let trans = ClewdrTransformer::new(config);

    // Perform the external request
    let super_res = SUPER_CLIENT
        .get("https://api.claude.ai")
        .send()
        .await
        .unwrap(); // In production, handle this error gracefully

    // Spawn a task to handle the streaming transformation
    tokio::spawn(async move {
        let input_stream = super_res.bytes_stream();
        let output_stream = trans.transform_stream(input_stream);
        pin_mut!(output_stream);

        while let Some(result) = output_stream.next().await {
            // Simulate expensive work (optional, adjust as needed)
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

            // Send the chunk to the client
            let chunk = Bytes::from(result.unwrap()); // Convert String to Bytes
            if tx.send(Ok(chunk)).await.is_err() {
                info!("Client disconnected, cancelling task");
                break;
            }
        }
    });

    // Return the streaming body
    let response_stream = ReceiverStream::new(rx);
    Body::from_stream(response_stream)
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct ClientRequestInfo {
    #[serde(default)]
    temperature: Option<f64>,
    #[serde(default)]
    messages: Vec<Message>,
    #[serde(default)]
    model: String,
    #[serde(default)]
    stream: bool,
    #[serde(default)]
    max_tokens: Option<i64>,
    #[serde(default)]
    stop: Option<Vec<String>>,
    #[serde(default)]
    top_p: Option<f64>,
    #[serde(default)]
    top_k: Option<i64>,
}
impl ClientRequestInfo {
    fn sanitize_client_request(mut self) -> ClientRequestInfo {
        if let Some(ref mut temp) = self.temperature {
            *temp = temp.clamp(0.0, 1.0);
        }
        self
    }
}

#[derive(Debug, serde::Deserialize, serde::Serialize, PartialEq, Eq, Clone, PartialOrd, Ord)]
pub struct Message {
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub customname: Option<bool>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub strip: Option<bool>,
    #[serde(default)]
    pub jailbreak: Option<bool>,
    #[serde(default)]
    pub main: Option<bool>,
    #[serde(default)]
    pub discard: Option<bool>,
    #[serde(default)]
    pub merged: Option<bool>,
    #[serde(default)]
    pub personality: Option<bool>,
    #[serde(default)]
    pub scenario: Option<bool>,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct PromptsGroup {
    pub first_user: Option<Message>,
    pub first_system: Option<Message>,
    pub first_assistant: Option<Message>,
    pub last_user: Option<Message>,
    pub last_system: Option<Message>,
    pub last_assistant: Option<Message>,
}

pub enum RetryStrategy {
    Api,
    Renew,
    RetryRegen,
    CurrentRenew,
    CurrentContinue,
}

impl RetryStrategy {
    pub fn is_current(&self) -> bool {
        matches!(self, Self::CurrentRenew | Self::CurrentContinue)
    }
}

impl PromptsGroup {
    pub fn find(messages: &[Message]) -> PromptsGroup {
        Self {
            first_user: messages.iter().find(|m| m.role == "user").cloned(),
            first_system: messages
                .iter()
                .find(|m| m.role == "system" && m.content != "[Start a new chat]")
                .cloned(),
            first_assistant: messages.iter().find(|m| m.role == "assistant").cloned(),
            last_user: messages.iter().rfind(|m| m.role == "user").cloned(),
            last_system: messages
                .iter()
                .rfind(|m| m.role == "system" && m.content != "[Start a new chat]")
                .cloned(),
            last_assistant: messages.iter().rfind(|m| m.role == "assistant").cloned(),
        }
    }
}

impl Default for Message {
    fn default() -> Self {
        Self {
            role: "user".to_string(),
            content: "".to_string(),
            customname: None,
            name: None,
            strip: None,
            jailbreak: None,
            main: None,
            discard: None,
            merged: None,
            personality: None,
            scenario: None,
        }
    }
}

pub async fn completion(
    State(state): State<AppState>,
    header: HeaderMap,
    Json(payload): Json<ClientRequestInfo>,
) -> Body {
    let b = state.try_completion(payload).await.unwrap();
    b
}

impl AppState {
    async fn try_completion(&self, mut payload: ClientRequestInfo) -> Result<Body, ClewdrError> {
        // TODO: 3rd key, API key, auth token, etc.
        let s = self.0.as_ref();
        let p = payload.sanitize_client_request();
        *s.model.write() = if s.is_pro.read().is_some() {
            Some(p.model.replace("--force", "").trim().to_string())
        } else {
            s.cookie_model.read().clone()
        };
        if s.uuid_org.read().is_empty() {
            // TODO: more keys
            return Err(ClewdrError::NoValidKey);
        }
        if !*s.changing.read()
            && s.is_pro.read().is_none()
            && *s.model.read() != *s.cookie_model.read()
        {
            self.cookie_changer(None, None);
            self.wait_for_change().await;
        }
        if p.messages.is_empty() {
            return Err(ClewdrError::WrongCompletionFormat);
        }
        if !p.stream && p.messages.len() == 1 && p.messages.first() == Some(&TEST_MESSAGE) {
            return Ok(Body::from(
                json!({
                    "choices":[
                        {
                            "message":{
                                "content": TITLE
                            }
                        }
                    ]
                })
                .to_string(),
            ));
        }
        if !p.stream && p.messages.first().map(|f|f.content.starts_with("From the list below, choose a word that best represents a character's outfit description, action, or emotion in their dialogue")).unwrap_or_default() {
            return Ok(Body::from(
                json!({
                    "choices":[
                        {
                            "message":{
                                "content": "neutral"
                            }
                        }
                    ]
                })
                .to_string(),
            ));
        }
        //  TODO: warn sample config
        if !s.model_list.read().contains(&p.model) && !p.model.contains("claude-") {
            return Err(ClewdrError::InvalidModel(p.model));
        }
        let current_prompts = PromptsGroup::find(&p.messages);
        let previous_prompts = PromptsGroup::find(&s.prev_messages.read());
        let same_prompts = {
            let mut a = p
                .messages
                .iter()
                .filter(|m| m.role != "system")
                .collect::<Vec<_>>();
            a.sort();
            let b = s.prev_messages.read();
            let mut b = b.iter().filter(|m| m.role != "system").collect::<Vec<_>>();
            b.sort();
            a == b
        };
        let same_char_diff_chat = !same_prompts
            && current_prompts.first_system.map(|s| s.content)
                == previous_prompts.first_system.map(|s| s.content)
            && current_prompts.first_user.map(|s| s.content)
                == previous_prompts.first_user.map(|s| s.content);
        let should_renew = s.config.read().settings.renew_always
            || s.conv_uuid.read().is_none()
            || *s.prev_impersonated.read()
            || (!s.config.read().settings.renew_always && same_prompts)
            || same_char_diff_chat;
        let retry_regen = s.config.read().settings.retry_regenerate
            && same_prompts
            && s.conv_char.read().is_some();
        if !same_prompts {
            *s.prev_messages.write() = p.messages.clone();
        }
        let r#type;
        // TODO: handle api key
        //TODO: handle retry regeneration and not same prompts
        if let Some(uuid) = s.conv_uuid.read().clone() {
            self.delete_chat(uuid).await?;
        }
        *s.conv_uuid.write() = Some(uuid::Uuid::new_v4().to_string());
        *s.conv_depth.write() = 0;
        let endpoint = if s.config.read().rproxy.is_empty() {
            ENDPOINT.to_string()
        } else {
            s.config.read().rproxy.clone()
        };
        let endpoint = format!(
            "{}/api/organizations/{}/chat_conversations",
            endpoint,
            s.uuid_org.read()
        );
        let body = json!({
            "uuid": s.conv_uuid.read().as_ref().unwrap(),
            "name":""
        });
        let res = SUPER_CLIENT
            .post(endpoint)
            .json(&body)
            .header_append(ORIGIN, ENDPOINT)
            .header_append(REFERER, header_ref(""))
            .header_append(COOKIE, self.header_cookie())
            .send()
            .await?;
        self.update_cookie_from_res(&res);
        check_res_err(res, &mut None).await?;
        r#type = RetryStrategy::Renew;
        // TODO: generate prompts
        let (prompt, systems) = self.handle_messages(&p.messages, r#type);
        let legacy = {
            let re = RegexBuilder::new(r"claude-([12]|instant)")
                .case_insensitive(true)
                .build()
                .unwrap();
            re.is_match(&p.model)
        };
        let messages_api = {
            // TODO: third key
            let re = RegexBuilder::new(r"<\|completeAPI\|>")
                .case_insensitive(true)
                .build()
                .unwrap();
            let re2 = Regex::new(r"<\|messagesAPI\|>").unwrap();
            !(legacy || re.is_match(&prompt)) || re2.is_match(&prompt)
        };
        let messages_log = {
            let re = Regex::new(r"<\|messagesLog\|>").unwrap();
            re.is_match(&prompt)
        };
        let fusion = {
            let re = Regex::new(r"<\|Fusion Mode\|>").unwrap();
            messages_api && re.is_match(&prompt)
        };
        let wedge = "\r";
        let stop_set = {
            let re = Regex::new(r"<\|stopSet *(\[.*?\]) *\|>").unwrap();
            re.find_iter(&prompt).nth(1)
        };
        let stop_revoke = {
            let re = Regex::new(r"<\|stopRevoke *(\[.*?\]) *\|>").unwrap();
            re.find_iter(&prompt).nth(1)
        };
        let stop_set: Vec<String> = stop_set
            .and_then(|s| serde_json::from_str(s.as_str()).ok())
            .unwrap_or_default();
        let stop_revoke: Vec<String> = stop_revoke
            .and_then(|s| serde_json::from_str(s.as_str()).ok())
            .unwrap_or_default();
        let stop = stop_set
            .into_iter()
            .chain(p.stop.unwrap_or_default().into_iter())
            .chain(["\n\nHuman:".into(), "\n\nAssistant:".into()])
            .filter(|s| {
                let s = s.trim();
                !s.is_empty() && !stop_revoke.iter().any(|r| r.eq_ignore_ascii_case(s))
            })
            .collect::<Vec<_>>();
        // TODO: Api key
        
        unimplemented!()
    }
}
