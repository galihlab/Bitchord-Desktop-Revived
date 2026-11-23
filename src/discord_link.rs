//! Discord Rich Presence through the account itself ("Discord via link"): the
//! gateway is opened with the account's own token, so the card shows on the
//! user's real profile, with working buttons and artwork pulled in from any
//! https image. A port of ForkChord's `core/src/discord.rs` (which ports
//! `com.my.kizzy.gateway.DiscordWebSocket` and `DiscordRPC.updateSong`).
//!
//! This is user-token automation, as on the phone: there is no API for a
//! third-party app to set a user's presence, so the token is read from a
//! discord.com page in the sign-in window (login.rs, the fresh-iframe trick)
//! only when the user clicks Connect. The token lives in the vault, never in
//! the settings file.
//!
//! The card itself is the one `crate::discord` builds for the plain IPC path,
//! from the same Settings wording, so a linked listener's card is laid out the
//! same way — only through their own account, with the artwork mirrored onto
//! Discord's CDN. The plain IPC presence stays as the fallback for a listener
//! who doesn't link an account.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;

/// BitChord's own Discord application (DiscordApplication.kt): what buttons and
/// the external-artwork mirror are filed under.
pub const APPLICATION_ID: &str = "1543679166967193620";
const GATEWAY: &str = "wss://gateway.discord.gg/?v=9&encoding=json";
const API: &str = "https://discord.com/api/v9";

const INITIAL_RECONNECT_MS: u64 = 1_000;
const MAX_RECONNECT_MS: u64 = 60_000;
/// A cleared card with nothing new for this long: the socket goes too, so the
/// account isn't shown online by a music app that isn't playing anything.
const IDLE_CLOSE_MS: u64 = 5 * 60_000;

const OP_DISPATCH: u8 = 0;
const OP_HEARTBEAT: u8 = 1;
const OP_IDENTIFY: u8 = 2;
const OP_PRESENCE_UPDATE: u8 = 3;
const OP_RESUME: u8 = 6;
const OP_RECONNECT: u8 = 7;
const OP_INVALID_SESSION: u8 = 9;
const OP_HELLO: u8 = 10;
const OP_HEARTBEAT_ACK: u8 = 11;

// ---- How the card reads -------------------------------------------------------

/// The linked account, as Settings shows it (KizzyRPC.getUserInfo).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct User {
    pub name: String,
    pub username: String,
    pub avatar_url: Option<String>,
}

/// How the gateway is doing, for the Settings row: off | connecting | ready | error.
pub type Status = (&'static str, Option<String>);

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Which line the member list repeats, as Discord's number for it (the IPC payload's
/// `StatusDisplayType`, which the gateway takes too).
fn status_display_type(s: crate::discord::Status) -> u8 {
    match s {
        crate::discord::Status::Name => 0,
        crate::discord::Status::Line1 => 2,
        crate::discord::Status::Line2 => 1,
    }
}

fn gateway_activity(c: &crate::discord::Card, large: Option<String>, small: Option<String>) -> Value {
    use crate::discord::{Kind, field};
    let mut activity = json!({
        "name": field(c.name()),
        "type": match c.kind { Kind::Listening => 2, Kind::Playing => 0 },
        "status_display_type": status_display_type(c.status),
        "url": Value::Null,
    });
    if let Some(l) = &c.lines[0] {
        activity["details"] = json!(field(&l.text));
        activity["details_url"] = json!(l.url);
    }
    if let Some(l) = &c.lines[1] {
        activity["state"] = json!(field(&l.text));
        activity["state_url"] = json!(l.url);
    }
    if let Some((start, end)) = c.times {
        activity["timestamps"] = json!({ "start": start });
        if let Some(end) = end {
            activity["timestamps"]["end"] = json!(end);
        }
    }
    if let Some(large) = large {
        let mut assets = json!({ "large_image": large });
        if let Some(text) = c.large_text() {
            assets["large_text"] = json!(field(text));
        }
        if let Some(url) = &c.art_url {
            assets["large_url"] = json!(url);
        }
        if let Some(small) = small {
            assets["small_image"] = json!(small);
            if let Some(text) = &c.small_text {
                assets["small_text"] = json!(field(text));
            }
        }
        activity["assets"] = assets;
    }
    if activity.get("assets").is_some() || !c.buttons.is_empty() {
        activity["application_id"] = json!(APPLICATION_ID);
    }
    if !c.buttons.is_empty() {
        activity["buttons"] = json!(c.buttons.iter().map(|(label, _)| label).collect::<Vec<_>>());
        activity["metadata"] = json!({ "button_urls": c.buttons.iter().map(|(_, url)| url).collect::<Vec<_>>() });
    }
    activity
}

/// Identify.toIdentifyPayload, with the web client's properties.
fn identify_payload(token: &str) -> Value {
    json!({
        "token": token,
        "capabilities": 16381,
        "properties": { "os": "Linux", "browser": "Chrome", "device": "" },
        "presence": { "status": "online", "since": 0, "activities": [], "afk": false },
        "compress": false,
        "client_state": {
            "guild_versions": {},
            "highest_last_message_id": "0",
            "read_state_version": 0,
            "user_guild_settings_version": -1,
            "user_settings_version": -1,
            "private_channels_version": "0",
            "api_code_version": 0,
        },
    })
}

fn heartbeat_frame(seq: u64) -> Value {
    json!({ "op": OP_HEARTBEAT, "d": if seq > 0 { Value::from(seq) } else { Value::Null } })
}

// ---- The gateway ---------------------------------------------------------------

enum Command {
    SetPresence(Value),
    ClearActivity,
}

#[derive(Default)]
struct Shared {
    sequence: u64,
    session_id: Option<String>,
    resume_url: Option<String>,
    /// Replayed onto every new session: Discord discards a presence with the
    /// session that set it.
    last_presence: Option<Value>,
    /// What this session already has, so the replay and a push don't both send it.
    delivered: Option<String>,
    ready: bool,
}

impl Shared {
    fn forget_session(&mut self) {
        self.resume_url = None;
        self.session_id = None;
        self.sequence = 0;
    }
}

fn rand_u64(min: u64, max: u64) -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    min + nanos % (max - min).max(1)
}

/// The gateway (DiscordWebSocket.kt): connect, pump, back off 1 s doubling to
/// 60 s, the heartbeat watchdog that drops a socket whose heartbeat went
/// unanswered, and the replay of the last presence onto a new session. The
/// socket lives on the backend's runtime; dropping the task closes it.
struct Gateway {
    shared: Arc<Mutex<Shared>>,
    commands: tokio::sync::mpsc::UnboundedSender<Command>,
    handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
}
enum Ended {
    /// Discord said the token is no good: stop trying, say so.
    Refused,
    /// Discord asked for an immediate reconnect (OP_RECONNECT).
    RetryNow,
    /// Anything else: back off and dial again.
    Backoff,
}

impl Gateway {
    fn spawn(
        backend: &crate::backend::Backend,
        token: String,
        status: Arc<dyn Fn(Status) + Send + Sync>,
    ) -> Arc<Gateway> {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (commands, command_rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = backend.spawn_handle(Self::run(token, shared.clone(), command_rx, status));
        Arc::new(Gateway {
            shared,
            commands,
            handle: Mutex::new(Some(handle)),
        })
    }

    fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    async fn close(&self) {
        if let Some(handle) = self.handle.lock().await.take() {
            handle.abort();
        }
        *self.shared.lock().await = Shared::default();
    }

    async fn run(
        token: String,
        shared: Arc<Mutex<Shared>>,
        mut commands: tokio::sync::mpsc::UnboundedReceiver<Command>,
        status: Arc<dyn Fn(Status) + Send + Sync>,
    ) {
        let report = |s: Status| status(s);
        let mut backoff = INITIAL_RECONNECT_MS;
        'outer: loop {
            report(("connecting", None));
            let url = shared
                .lock()
                .await
                .resume_url
                .clone()
                .unwrap_or_else(|| GATEWAY.to_string());
            // Through the proxy setting, like every other socket (see jam.rs).
            let connect = async {
                let u = url::Url::parse(&url)?;
                let host = u.host_str().unwrap_or_default().to_owned();
                let port = u.port_or_known_default().unwrap_or(443);
                let tcp = crate::proxy::connect(&host, port).await?;
                anyhow::Ok(tokio_tungstenite::client_async_tls(url, tcp).await?)
            };
            let socket = match tokio::time::timeout(Duration::from_secs(15), connect).await {
                Ok(Ok(pair)) => pair.0,
                Ok(Err(e)) => {
                    log::debug!("discord gateway: {e}");
                    tokio::time::sleep(Duration::from_millis(backoff)).await;
                    backoff = (backoff * 2).min(MAX_RECONNECT_MS);
                    continue;
                }
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(backoff)).await;
                    backoff = (backoff * 2).min(MAX_RECONNECT_MS);
                    continue;
                }
            };
            backoff = INITIAL_RECONNECT_MS;
            let (mut write, mut read) = socket.split();
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Message>();
            let writer = tokio::spawn(async move {
                while let Some(message) = rx.recv().await {
                    if write.send(message).await.is_err() {
                        break;
                    }
                }
            });

            let mut awaiting_ack = false;
            let mut heartbeat_interval: u64 = 0;
            let mut opened = false;
            let mut ended = Ended::Backoff;
            // Discord asks for the first heartbeat after a random fraction of the interval.
            let mut next_beat = tokio::time::Instant::now() + Duration::from_millis(1_000);

            loop {
                tokio::select! {
                    command = commands.recv() => {
                        let Some(command) = command else { break };
                        let presence = match command {
                            Command::SetPresence(presence) => Some(presence),
                            Command::ClearActivity => Some(json!({ "activities": [], "afk": true, "since": 0, "status": "online" })),
                        };
                        let presence = presence.expect("both arms produce one");
                        let mut guard = shared.lock().await;
                        guard.last_presence = Some(presence.clone());
                        if guard.ready {
                            let key = presence.to_string();
                            if guard.delivered.as_deref() != Some(key.as_str()) {
                                let frame = Message::text(json!({ "op": OP_PRESENCE_UPDATE, "d": presence }).to_string());
                                if tx.send(frame).is_ok() {
                                    guard.delivered = Some(key);
                                }
                            }
                        }
                    }
                    frame = read.next() => {
                        let Some(frame) = frame else { break };
                        let Ok(frame) = frame else { break };
                        match frame {
                            Message::Close(close) => {
                                // 4004: authentication failed. Retrying with the same token never helps.
                                if close.as_ref().map(|f| u16::from(f.code)) == Some(4004) {
                                    ended = Ended::Refused;
                                }
                                break;
                            }
                            Message::Text(text) => {
                                let Ok(payload) = serde_json::from_str::<Value>(&text) else { continue };
                                if let Some(s) = payload.get("s").and_then(|s| s.as_u64()) {
                                    shared.lock().await.sequence = s;
                                }
                                let op = payload.get("op").and_then(|o| o.as_u64()).unwrap_or(u64::MAX) as u8;
                                match op {
                                    OP_DISPATCH => {
                                        if payload.get("t").and_then(|t| t.as_str()) == Some("READY") {
                                            let d = payload.get("d").cloned().unwrap_or(Value::Null);
                                            let mut guard = shared.lock().await;
                                            guard.session_id = d.get("session_id").and_then(|s| s.as_str()).map(|s| s.to_string());
                                            guard.resume_url = d.get("resume_gateway_url").and_then(|r| r.as_str()).map(|r| format!("{r}/?v=9&encoding=json"));
                                            guard.ready = true;
                                            drop(guard);
                                            opened = true;
                                            awaiting_ack = false;
                                            report(("ready", None));
                                            let replay = shared.lock().await.last_presence.clone();
                                            if let Some(presence) = replay {
                                                let key = presence.to_string();
                                                let frame = Message::text(json!({ "op": OP_PRESENCE_UPDATE, "d": presence }).to_string());
                                                if tx.send(frame).is_ok() {
                                                    shared.lock().await.delivered = Some(key);
                                                }
                                            }
                                        }
                                    }
                                    OP_HELLO => {
                                        heartbeat_interval = payload.pointer("/d/heartbeat_interval").and_then(|i| i.as_u64()).unwrap_or(41_250);
                                        let (seq, session) = {
                                            let guard = shared.lock().await;
                                            (guard.sequence, guard.session_id.clone())
                                        };
                                        let out = if seq > 0 {
                                            json!({ "op": OP_RESUME, "d": { "token": token, "session_id": session, "seq": seq } })
                                        } else {
                                            json!({ "op": OP_IDENTIFY, "d": identify_payload(&token) })
                                        };
                                        let _ = tx.send(Message::text(out.to_string()));
                                        next_beat = tokio::time::Instant::now() + Duration::from_millis(heartbeat_interval * (rand_u64(1000, 2000) / 1000).max(1));
                                    }
                                    OP_HEARTBEAT => {
                                        let seq = shared.lock().await.sequence;
                                        let _ = tx.send(Message::text(heartbeat_frame(seq).to_string()));
                                    }
                                    OP_HEARTBEAT_ACK => {
                                        awaiting_ack = false;
                                    }
                                    OP_RECONNECT => {
                                        ended = Ended::RetryNow;
                                        break;
                                    }
                                    OP_INVALID_SESSION => {
                                        shared.lock().await.forget_session();
                                        let _ = tx.send(Message::text(json!({ "op": OP_IDENTIFY, "d": identify_payload(&token) }).to_string()));
                                    }
                                    _ => {}
                                }
                            }
                            _ => {}
                        }
                    }
                    _ = tokio::time::sleep_until(next_beat) => {
                        // The watchdog: a heartbeat unanswered for a whole interval means the socket is gone.
                        if opened {
                            if awaiting_ack {
                                ended = Ended::Backoff;
                                break;
                            }
                            awaiting_ack = true;
                            let seq = shared.lock().await.sequence;
                            let _ = tx.send(Message::text(heartbeat_frame(seq).to_string()));
                        }
                        next_beat = tokio::time::Instant::now() + Duration::from_millis(if heartbeat_interval > 0 { heartbeat_interval } else { 41_250 });
                    }
                }
            }

            writer.abort();
            {
                let mut guard = shared.lock().await;
                guard.ready = false;
                guard.delivered = None;
                // An attempt that never opened is as often a stale resume URL as a dead network.
                if !opened {
                    guard.forget_session();
                }
            }
            match ended {
                Ended::Refused => {
                    report((
                        "error",
                        Some("Discord refused the token. Connect the account again.".into()),
                    ));
                    return;
                }
                Ended::RetryNow => continue,
                Ended::Backoff => {
                    report(("connecting", None));
                    tokio::time::sleep(Duration::from_millis(backoff)).await;
                    backoff = (backoff * 2).min(MAX_RECONNECT_MS);
                    continue 'outer;
                }
            }
        }
    }
}

// ---- The service ---------------------------------------------------------------

/// One linked Discord account's presence. The gateway is brought up for the
/// first card, kept while music plays, and dropped a while after the last
/// cleared one (an idle app shouldn't hold the account online).
pub struct Service {
    backend: crate::backend::Backend,
    http: reqwest::Client,
    gateway: Arc<Mutex<Option<Arc<Gateway>>>>,
    status: Arc<std::sync::Mutex<Status>>,
    /// Holds the idle close so a new activity can cancel it.
    idle: Arc<Mutex<Option<tokio::task::AbortHandle>>>,
    /// Discord's CDN mirror of an https image, as an `mp:` asset (ExternalAssets.kt),
    /// cached: the same cover is asked for on every card.
    artwork: Mutex<HashMap<String, (Option<String>, Instant)>>,
    revision: AtomicU64,
    apply: Mutex<()>,
}

impl Service {
    pub fn new(backend: &crate::backend::Backend) -> Self {
        let http = crate::proxy::apply(reqwest::Client::builder())
            .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36")
            .build()
            .expect("discord http client");
        Self {
            backend: backend.clone(),
            http,
            gateway: Arc::new(Mutex::new(None)),
            status: Arc::new(std::sync::Mutex::new(("off", None))),
            idle: Arc::new(Mutex::new(None)),
            artwork: Mutex::new(HashMap::new()),
            revision: AtomicU64::new(0),
            apply: Mutex::new(()),
        }
    }

    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }

    /// ExternalAssets.kt: Discord's CDN mirror of an http image, as an `mp:` asset;
    /// cached (ArtworkCache). A mirror that wouldn't come keeps the raw URL, as
    /// RpcImage.ExternalImage does.
    async fn external_asset(&self, token: &str, image_url: &str) -> Option<String> {
        let mut cache = self.artwork.lock().await;
        if let Some((asset, _)) = cache.get(image_url).filter(|(asset, at)| asset.is_some() || at.elapsed() < Duration::from_secs(60)) {
            return asset.clone();
        }
        let result = async {
            let response = self
                .http
                .post(format!(
                    "{API}/applications/{APPLICATION_ID}/external-assets"
                ))
                .header("Authorization", token)
                .timeout(Duration::from_secs(6))
                .json(&json!({ "urls": [image_url] }))
                .send()
                .await
                .ok()?
                .error_for_status()
                .ok()?
                .json::<Value>()
                .await
                .ok()?;
            let path = response
                .as_array()?
                .first()?
                .get("external_asset_path")?
                .as_str()?
                .to_string();
            Some(format!("mp:{path}"))
        }
        .await;
        if cache.len() >= 256
            && let Some(key) = cache.iter().min_by_key(|(_, (_, at))| *at).map(|(url, _)| url.clone())
        {
            cache.remove(&key);
        }
        cache.insert(image_url.to_string(), (result.clone(), Instant::now()));
        result
    }

    async fn image(&self, token: &str, picture: &crate::discord_artwork::Picture) -> Option<String> {
        let url = picture.url.as_deref()?;
        if let Some(asset) = self.external_asset(token, url).await {
            return Some(asset);
        }
        if let Some(still) = &picture.fallback {
            return Some(self.external_asset(token, still).await.unwrap_or_else(|| still.clone()));
        }
        Some(url.to_owned())
    }

    async fn build_presence(&self, token: &str, card: &crate::discord::Card) -> Value {
        let (large, small) = match (&card.art, &card.small_art) {
            (Some(large), Some(small)) => {
                let (large, small) = tokio::join!(self.image(token, large), self.image(token, small));
                (large, small)
            }
            (Some(large), None) => (self.image(token, large).await, None),
            _ => (None, None),
        };
        json!({
            "activities": [gateway_activity(card, large, small)],
            "afk": true,
            "since": now_ms(),
            "status": "online",
        })
    }

    /// The page reports what's playing; `None` takes the card down but leaves the
    /// socket up for a while (an idle close puts the socket away later, so an idle
    /// app doesn't hold the account online).
    pub fn set_presence(self: &Arc<Self>, token: String, card: Option<crate::discord::Card>) {
        let revision = self.revision.fetch_add(1, Ordering::AcqRel) + 1;
        let this = self.clone();
        self.backend.spawn(async move {
            let guard = this.apply.lock().await;
            if this.revision.load(Ordering::Acquire) != revision {
                return;
            }
            if let Some(handle) = this.idle.lock().await.take() {
                handle.abort();
            }
            let Some(card) = card else {
                if let Some(gateway) = this.gateway.lock().await.clone() {
                    gateway.send(Command::ClearActivity);
                }
                let idle = this.clone();
                let handle = this.backend.spawn_handle(async move {
                    tokio::time::sleep(Duration::from_millis(IDLE_CLOSE_MS)).await;
                    let _guard = idle.apply.lock().await;
                    if idle.revision.load(Ordering::Acquire) == revision {
                        // Take our own handle out before close_gateway aborts pending idle work.
                        idle.idle.lock().await.take();
                        idle.close_gateway().await;
                    }
                });
                *this.idle.lock().await = Some(handle.abort_handle());
                return;
            };
            let gateway = {
                let mut current = this.gateway.lock().await;
                match current.as_ref() {
                    Some(existing) => existing.clone(),
                    None => {
                        let backend = this.backend.clone();
                        let status_shared = this.status.clone();
                        let status: Arc<dyn Fn(Status) + Send + Sync> =
                            Arc::new(move |s: Status| {
                                *status_shared.lock().unwrap() = s;
                                backend.wake();
                            });
                        let gateway = Gateway::spawn(&this.backend, token.clone(), status);
                        *current = Some(gateway.clone());
                        gateway
                    }
                }
            };
            drop(guard);
            let presence = this.build_presence(&token, &card).await;
            let _guard = this.apply.lock().await;
            // A slow image proxy must never put an old song back after a skip or clear.
            // The gateway already replays its last presence when a session opens.
            if this.revision.load(Ordering::Acquire) == revision {
                gateway.send(Command::SetPresence(presence));
            }
        });
    }

    /// Drop the socket (and forget it): the next card brings it back up.
    pub fn shutdown(self: &Arc<Self>) {
        self.revision.fetch_add(1, Ordering::AcqRel);
        let this = self.clone();
        self.backend.spawn(async move {
            let _guard = this.apply.lock().await;
            this.close_gateway().await;
        });
    }

    /// Drop the socket (and forget it): the next card brings it back up.
    async fn close_gateway(&self) {
        if let Some(handle) = self.idle.lock().await.take() {
            handle.abort();
        }
        if let Some(gateway) = self.gateway.lock().await.take() {
            gateway.close().await;
        }
        *self.status.lock().unwrap() = ("off", None);
        self.artwork.lock().await.clear();
        self.backend.wake();
    }

    /// Where the linked account's gateway stands.
    pub async fn user_info(&self, token: &str) -> anyhow::Result<User> {
        let response = self
            .http
            .get(format!("{API}/users/@me"))
            .header("Authorization", token)
            .send()
            .await?;
        if response.status().as_u16() == 401 {
            anyhow::bail!("Discord refused the token");
        }
        let me: Value = response.json().await?;
        let id = me
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let username = me
            .get("username")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let name = me
            .get("global_name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(&username)
            .to_string();
        let avatar_url = me
            .get("avatar")
            .and_then(|v| v.as_str())
            .map(|avatar| format!("https://cdn.discordapp.com/avatars/{id}/{avatar}.png"));
        Ok(User {
            name,
            username,
            avatar_url,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_card_preserves_small_image_links_and_elapsed_time() {
        let mut c = crate::discord::Card::default();
        c.kind = crate::discord::Kind::Playing;
        c.name = Some("BitChord Test".into());
        c.status = crate::discord::Status::Line1;
        c.lines[0] = Some(crate::discord::Line { text: "Song".into(), url: Some("https://example.com/song".into()) });
        c.lines[2] = Some(crate::discord::Line { text: "Cover".into(), url: None });
        c.art_url = Some("https://example.com/cover".into());
        c.small_text = Some("Paused".into());
        c.times = Some((50, None));
        c.buttons = vec![("Link".into(), "https://example.com".into())];
        let activity = gateway_activity(&c, Some("mp:large".into()), Some("mp:small".into()));
        assert_eq!(activity["application_id"], APPLICATION_ID);
        assert_eq!(activity["type"], 0);
        assert_eq!(activity["status_display_type"], 2);
        assert_eq!(activity["timestamps"], json!({"start": 50}));
        assert_eq!(activity["details_url"], "https://example.com/song");
        assert_eq!(
            activity["assets"],
            json!({
                "large_image": "mp:large",
                "large_text": "Cover",
                "large_url": "https://example.com/cover",
                "small_image": "mp:small",
                "small_text": "Paused",
            })
        );
        assert_eq!(activity["buttons"], json!(["Link"]));
        assert_eq!(activity["metadata"]["button_urls"], json!(["https://example.com"]));
        assert!(gateway_activity(&c, None, Some("mp:small".into())).get("assets").is_none());

        c.annotation = Some("FLAC · 44.1kHz".into());
        c.lines[1] = Some(crate::discord::Line { text: "Artist".into(), url: Some("https://example.com/artist".into()) });
        let updated = gateway_activity(&c, Some("mp:large".into()), None);
        assert_eq!(updated["state"], "Artist");
        assert_eq!(updated["state_url"], "https://example.com/artist");
        assert_eq!(updated["assets"]["large_text"], "FLAC · 44.1kHz");
    }

    #[test]
    fn the_member_list_line_is_discords_number_for_it() {
        assert_eq!(status_display_type(crate::discord::Status::Name), 0);
        assert_eq!(status_display_type(crate::discord::Status::Line1), 2);
        assert_eq!(status_display_type(crate::discord::Status::Line2), 1);
    }

    #[test]
    fn identify_carries_the_web_properties() {
        let payload = identify_payload("tok");
        assert_eq!(payload["token"], "tok");
        assert_eq!(payload["properties"]["browser"], "Chrome");
        assert_eq!(payload["client_state"]["highest_last_message_id"], "0");
    }

    #[test]
    fn heartbeat_reports_the_sequence() {
        assert_eq!(heartbeat_frame(0)["d"], Value::Null);
        assert_eq!(heartbeat_frame(42)["d"], 42);
    }
}
