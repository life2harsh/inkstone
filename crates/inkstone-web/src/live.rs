//! Live mode: pen strokes and cursors go straight to the other devices on
//! the note over WebRTC data channels that never wait for a lost packet
//! (unordered, no retransmits; SCTP over DTLS over UDP). The handshake rides
//! on encrypted presence, so the server only relays ciphertext, and every
//! frame on a channel is the same encrypted presence the socket carries.
//! Edits never use these channels: they stay on the reliable socket.
//!
//! Who calls whom: a device with Live mode on says hello. A device that
//! hears a hello calls back unless it is also live and has the higher id,
//! in which case it says hello to the caller-to-be instead.

use std::collections::HashMap;

use uuid::Uuid;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    RtcConfiguration, RtcDataChannel, RtcDataChannelEvent, RtcDataChannelInit, RtcDataChannelState,
    RtcIceCandidateInit, RtcIceServer, RtcPeerConnection, RtcPeerConnectionIceEvent,
    RtcPeerConnectionState, RtcSdpType, RtcSessionDescriptionInit,
};

use crate::{Driver, EncryptedPresenceWire, PRESENCE_VERSION, PresenceKind, PresencePayload};

const LIVE_KEY: &str = "inkstone-live-mode";
/// Least time between hellos to one device.
const HELLO_GAP_MS: f64 = 4_000.0;
/// Least time between direct stroke updates; starts and ends always go.
const DIRECT_GAP_MS: f64 = 8.0;
/// A channel this far behind is congested: skip frames rather than queue
/// them, so what arrives is current.
const MAX_BUFFERED: u32 = 64 * 1024;
const ICE_SERVERS: [&str; 2] = [
    "stun:stun.cloudflare.com:3478",
    "stun:stun.l.google.com:19302",
];

pub(crate) fn enabled() -> bool {
    web_sys::window()
        .and_then(|window| window.local_storage().ok().flatten())
        .and_then(|storage| storage.get_item(LIVE_KEY).ok().flatten())
        .is_some_and(|value| value == "on")
}

fn set_enabled(on: bool) {
    if let Some(storage) =
        web_sys::window().and_then(|window| window.local_storage().ok().flatten())
    {
        let _ = storage.set_item(LIVE_KEY, if on { "on" } else { "off" });
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "signal", rename_all = "snake_case")]
pub(crate) enum LiveSignal {
    Hello {
        live: bool,
    },
    Offer {
        sdp: String,
    },
    Answer {
        sdp: String,
    },
    Ice {
        candidate: String,
        sdp_mid: Option<String>,
        sdp_m_line_index: Option<u16>,
    },
    Bye,
}

/// Whether this device places the call to `remote`, which said hello.
fn should_call(own_live: bool, own: Uuid, remote: Uuid) -> bool {
    !own_live || own < remote
}

struct Peer {
    connection: RtcPeerConnection,
    channel: Option<RtcDataChannel>,
    remote_ready: bool,
    pending_ice: Vec<RtcIceCandidateInit>,
    _callbacks: Vec<Closure<dyn FnMut(JsValue)>>,
}

impl Peer {
    fn close(self) {
        if let Some(channel) = &self.channel {
            channel.set_onopen(None);
            channel.set_onmessage(None);
            channel.set_onclose(None);
            channel.close();
        }
        self.connection.set_onicecandidate(None);
        self.connection.set_ondatachannel(None);
        self.connection.set_onconnectionstatechange(None);
        self.connection.close();
    }

    fn open_channel(&self) -> Option<&RtcDataChannel> {
        self.channel
            .as_ref()
            .filter(|channel| channel.ready_state() == RtcDataChannelState::Open)
    }
}

#[derive(Default)]
pub(crate) struct LiveMesh {
    peers: HashMap<Uuid, Peer>,
    direct_counters: HashMap<Uuid, u64>,
    last_hello: HashMap<Uuid, f64>,
    last_direct_send: f64,
    /// Frames accepted from direct channels, for tests.
    received: u64,
}

impl LiveMesh {
    pub(crate) fn direct_counter(&self, sender: Uuid) -> Option<u64> {
        self.direct_counters.get(&sender).copied()
    }

    pub(crate) fn note_direct_counter(&mut self, sender: Uuid, counter: u64) {
        self.direct_counters.insert(sender, counter);
        self.received += 1;
    }

    pub(crate) fn received(&self) -> u64 {
        self.received
    }

    pub(crate) fn open_count(&self) -> usize {
        self.peers
            .values()
            .filter(|peer| peer.open_channel().is_some())
            .count()
    }

    fn take_peer(&mut self, id: Uuid) -> Option<Peer> {
        self.direct_counters.remove(&id);
        self.peers.remove(&id)
    }
}

fn session_description(kind: RtcSdpType, sdp: &str) -> RtcSessionDescriptionInit {
    let description = RtcSessionDescriptionInit::new(kind);
    description.set_sdp(sdp);
    description
}

fn sdp_of(value: &JsValue) -> Option<String> {
    js_sys::Reflect::get(value, &"sdp".into()).ok()?.as_string()
}

impl Driver {
    fn live_available(&self) -> bool {
        !self.0.borrow().local_mode && self.2.borrow().socket.is_some()
    }

    /// Number of devices this one reaches directly.
    pub(crate) fn live_direct_count(&self) -> usize {
        self.2.borrow().live.open_count()
    }

    pub(crate) fn toggle_live_mode(&self) {
        let on = !enabled();
        set_enabled(on);
        if on {
            self.2.borrow_mut().live.last_hello.clear();
            self.live_announce();
        } else {
            self.live_close_all(true);
        }
        self.render();
    }

    fn live_send_signal(&self, to: Option<Uuid>, signal: LiveSignal) {
        let payload = PresencePayload {
            version: PRESENCE_VERSION,
            sent_at_ms: js_sys::Date::now() as i64,
            state: PresenceKind::Signal { to, signal },
        };
        if let Some(frame) = self.seal_presence(payload) {
            self.send_presence_frame(&frame);
        }
    }

    /// Says hello to everyone on the note, at most every few seconds.
    pub(crate) fn live_announce(&self) {
        if !enabled() || !self.live_available() {
            return;
        }
        let now = js_sys::Date::now();
        {
            let mut handles = self.2.borrow_mut();
            let last = handles.live.last_hello.get(&Uuid::nil()).copied();
            if last.is_some_and(|last| now - last < HELLO_GAP_MS) {
                return;
            }
            handles.live.last_hello.insert(Uuid::nil(), now);
        }
        self.live_send_signal(None, LiveSignal::Hello { live: true });
    }

    fn live_hello_to(&self, remote: Uuid) {
        let now = js_sys::Date::now();
        {
            let mut handles = self.2.borrow_mut();
            let last = handles.live.last_hello.get(&remote).copied();
            if last.is_some_and(|last| now - last < HELLO_GAP_MS) {
                return;
            }
            handles.live.last_hello.insert(remote, now);
        }
        self.live_send_signal(Some(remote), LiveSignal::Hello { live: true });
    }

    /// Socket presence from `remote`: a live device we have no line to yet
    /// says hello so a channel forms.
    pub(crate) fn live_noticed(&self, remote: Uuid) {
        if enabled() && !self.2.borrow().live.peers.contains_key(&remote) {
            self.live_hello_to(remote);
        }
    }

    pub(crate) fn live_handle_signal(&self, remote: Uuid, signal: LiveSignal) {
        if !self.live_available() {
            return;
        }
        match signal {
            LiveSignal::Hello { live: remote_live } => {
                if self.2.borrow().live.peers.contains_key(&remote) {
                    return;
                }
                let own_live = enabled();
                if !own_live && !remote_live {
                    return;
                }
                let Some(own) = self.0.borrow().device_id else {
                    return;
                };
                if should_call(own_live, own, remote) {
                    self.live_call(remote);
                } else {
                    self.live_hello_to(remote);
                }
            }
            LiveSignal::Offer { sdp } => self.live_answer(remote, sdp),
            LiveSignal::Answer { sdp } => self.live_accept_answer(remote, sdp),
            LiveSignal::Ice {
                candidate,
                sdp_mid,
                sdp_m_line_index,
            } => {
                let init = RtcIceCandidateInit::new(&candidate);
                init.set_sdp_mid(sdp_mid.as_deref());
                init.set_sdp_m_line_index(sdp_m_line_index);
                let connection = {
                    let mut handles = self.2.borrow_mut();
                    let Some(peer) = handles.live.peers.get_mut(&remote) else {
                        return;
                    };
                    if !peer.remote_ready {
                        peer.pending_ice.push(init);
                        return;
                    }
                    peer.connection.clone()
                };
                let _ = connection.add_ice_candidate_with_opt_rtc_ice_candidate_init(Some(&init));
            }
            LiveSignal::Bye => self.live_close_peer(remote),
        }
    }

    fn live_new_peer(&self, remote: Uuid) -> Option<RtcPeerConnection> {
        self.live_close_peer(remote);
        let servers = js_sys::Array::new();
        for url in ICE_SERVERS {
            let server = RtcIceServer::new();
            server.set_urls_str(url);
            servers.push(&server);
        }
        let config = RtcConfiguration::new();
        config.set_ice_servers(&servers);
        let connection = RtcPeerConnection::new_with_configuration(&config).ok()?;

        let mut callbacks = Vec::new();
        let this = self.clone();
        let on_ice = Closure::wrap(Box::new(move |event: JsValue| {
            let Some(candidate) = event
                .dyn_into::<RtcPeerConnectionIceEvent>()
                .ok()
                .and_then(|event| event.candidate())
            else {
                return;
            };
            this.live_send_signal(
                Some(remote),
                LiveSignal::Ice {
                    candidate: candidate.candidate(),
                    sdp_mid: candidate.sdp_mid(),
                    sdp_m_line_index: candidate.sdp_m_line_index(),
                },
            );
        }) as Box<dyn FnMut(JsValue)>);
        connection.set_onicecandidate(Some(on_ice.as_ref().unchecked_ref()));
        callbacks.push(on_ice);

        let this = self.clone();
        let watched = connection.clone();
        let on_state = Closure::wrap(Box::new(move |_event: JsValue| {
            if matches!(
                watched.connection_state(),
                RtcPeerConnectionState::Failed | RtcPeerConnectionState::Closed
            ) {
                // Strokes fall back to the socket; a later hello retries.
                this.live_close_peer(remote);
                this.render();
            }
        }) as Box<dyn FnMut(JsValue)>);
        connection.set_onconnectionstatechange(Some(on_state.as_ref().unchecked_ref()));
        callbacks.push(on_state);

        let this = self.clone();
        let on_channel = Closure::wrap(Box::new(move |event: JsValue| {
            if let Ok(event) = event.dyn_into::<RtcDataChannelEvent>() {
                this.live_attach_channel(remote, event.channel());
            }
        }) as Box<dyn FnMut(JsValue)>);
        connection.set_ondatachannel(Some(on_channel.as_ref().unchecked_ref()));
        callbacks.push(on_channel);

        self.2.borrow_mut().live.peers.insert(
            remote,
            Peer {
                connection: connection.clone(),
                channel: None,
                remote_ready: false,
                pending_ice: Vec::new(),
                _callbacks: callbacks,
            },
        );
        Some(connection)
    }

    fn live_attach_channel(&self, remote: Uuid, channel: RtcDataChannel) {
        let this = self.clone();
        let on_open = Closure::wrap(Box::new(move |_event: JsValue| {
            this.render();
        }) as Box<dyn FnMut(JsValue)>);
        channel.set_onopen(Some(on_open.as_ref().unchecked_ref()));

        let this = self.clone();
        let on_message = Closure::wrap(Box::new(move |event: JsValue| {
            let Some(text) = event
                .dyn_into::<web_sys::MessageEvent>()
                .ok()
                .and_then(|event| event.data().as_string())
            else {
                return;
            };
            let Ok(frame) = serde_json::from_str::<EncryptedPresenceWire>(&text) else {
                return;
            };
            // Only the device on the other end may speak on its channel.
            if frame.sender_device_id != remote {
                return;
            }
            let Ok((workspace_id, key, device_id)) = this.socket_context() else {
                return;
            };
            let doc_id = this.0.borrow().selected_doc_id;
            this.handle_encrypted_presence(frame, workspace_id, doc_id, &key, device_id, true);
        }) as Box<dyn FnMut(JsValue)>);
        channel.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

        let this = self.clone();
        let on_close = Closure::wrap(Box::new(move |_event: JsValue| {
            this.live_close_peer(remote);
            this.render();
        }) as Box<dyn FnMut(JsValue)>);
        channel.set_onclose(Some(on_close.as_ref().unchecked_ref()));

        let mut handles = self.2.borrow_mut();
        if let Some(peer) = handles.live.peers.get_mut(&remote) {
            peer.channel = Some(channel);
            peer._callbacks.extend([on_open, on_message, on_close]);
        } else {
            channel.close();
        }
    }

    fn live_call(&self, remote: Uuid) {
        let Some(connection) = self.live_new_peer(remote) else {
            return;
        };
        let init = RtcDataChannelInit::new();
        init.set_ordered(false);
        init.set_max_retransmits(0);
        let channel = connection.create_data_channel_with_data_channel_dict("inkstone-live", &init);
        self.live_attach_channel(remote, channel);
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let Ok(offer) = JsFuture::from(connection.create_offer()).await else {
                this.live_close_peer(remote);
                return;
            };
            let Some(sdp) = sdp_of(&offer) else {
                return;
            };
            let description = session_description(RtcSdpType::Offer, &sdp);
            if JsFuture::from(connection.set_local_description(&description))
                .await
                .is_err()
            {
                this.live_close_peer(remote);
                return;
            }
            this.live_send_signal(Some(remote), LiveSignal::Offer { sdp });
        });
    }

    fn live_answer(&self, remote: Uuid, sdp: String) {
        let Some(connection) = self.live_new_peer(remote) else {
            return;
        };
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let offer = session_description(RtcSdpType::Offer, &sdp);
            if JsFuture::from(connection.set_remote_description(&offer))
                .await
                .is_err()
            {
                this.live_close_peer(remote);
                return;
            }
            this.live_remote_ready(remote);
            let Ok(answer) = JsFuture::from(connection.create_answer()).await else {
                this.live_close_peer(remote);
                return;
            };
            let Some(sdp) = sdp_of(&answer) else {
                return;
            };
            let description = session_description(RtcSdpType::Answer, &sdp);
            if JsFuture::from(connection.set_local_description(&description))
                .await
                .is_err()
            {
                this.live_close_peer(remote);
                return;
            }
            this.live_send_signal(Some(remote), LiveSignal::Answer { sdp });
        });
    }

    fn live_accept_answer(&self, remote: Uuid, sdp: String) {
        let Some(connection) = self
            .2
            .borrow()
            .live
            .peers
            .get(&remote)
            .filter(|peer| !peer.remote_ready)
            .map(|peer| peer.connection.clone())
        else {
            return;
        };
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let answer = session_description(RtcSdpType::Answer, &sdp);
            if JsFuture::from(connection.set_remote_description(&answer))
                .await
                .is_err()
            {
                this.live_close_peer(remote);
                return;
            }
            this.live_remote_ready(remote);
        });
    }

    /// The remote description is set: candidates that arrived early apply.
    fn live_remote_ready(&self, remote: Uuid) {
        let (connection, pending) = {
            let mut handles = self.2.borrow_mut();
            let Some(peer) = handles.live.peers.get_mut(&remote) else {
                return;
            };
            peer.remote_ready = true;
            (
                peer.connection.clone(),
                std::mem::take(&mut peer.pending_ice),
            )
        };
        for candidate in pending {
            let _ = connection.add_ice_candidate_with_opt_rtc_ice_candidate_init(Some(&candidate));
        }
    }

    pub(crate) fn live_close_peer(&self, remote: Uuid) {
        let peer = self.2.borrow_mut().live.take_peer(remote);
        if let Some(peer) = peer {
            peer.close();
        }
    }

    /// Drops every direct line, telling the others when `say_bye`.
    pub(crate) fn live_close_all(&self, say_bye: bool) {
        let peers = {
            let mut handles = self.2.borrow_mut();
            handles.live.direct_counters.clear();
            handles.live.last_hello.clear();
            std::mem::take(&mut handles.live.peers)
        };
        if peers.is_empty() {
            return;
        }
        if say_bye {
            self.live_send_signal(None, LiveSignal::Bye);
        }
        for (_, peer) in peers {
            peer.close();
        }
    }

    /// Sends presence over every open direct channel. Stroke updates are
    /// capped near 120 a second and skipped on a congested channel; starts
    /// and ends (`urgent`) always go, and the socket carries them as well.
    pub(crate) fn live_broadcast(&self, state: &PresenceKind, urgent: bool) {
        let now = js_sys::Date::now();
        {
            let mut handles = self.2.borrow_mut();
            if handles.live.open_count() == 0 {
                return;
            }
            if !urgent && now - handles.live.last_direct_send < DIRECT_GAP_MS {
                return;
            }
            handles.live.last_direct_send = now;
        }
        let payload = PresencePayload {
            version: PRESENCE_VERSION,
            sent_at_ms: now as i64,
            state: state.clone(),
        };
        let Some(frame) = self.seal_presence(payload) else {
            return;
        };
        let Ok(text) = serde_json::to_string(&frame) else {
            return;
        };
        let handles = self.2.borrow();
        for peer in handles.live.peers.values() {
            if let Some(channel) = peer.open_channel() {
                if urgent || channel.buffered_amount() < MAX_BUFFERED {
                    let _ = channel.send_with_str(&text);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exactly_one_side_calls() {
        let (low, high) = (Uuid::from_u128(1), Uuid::from_u128(2));
        // Both live: only the lower id calls.
        assert!(should_call(true, low, high));
        assert!(!should_call(true, high, low));
        // Only the hello's sender is live: the listener calls.
        assert!(should_call(false, high, low));
        assert!(should_call(false, low, high));
    }

    #[test]
    fn signals_round_trip() {
        let signal = LiveSignal::Ice {
            candidate: "candidate:1 1 udp 1 127.0.0.1 9 typ host".into(),
            sdp_mid: Some("0".into()),
            sdp_m_line_index: Some(0),
        };
        let json = serde_json::to_string(&signal).unwrap();
        assert!(json.contains("\"signal\":\"ice\""), "{json}");
        assert!(matches!(
            serde_json::from_str::<LiveSignal>(&json).unwrap(),
            LiveSignal::Ice { .. }
        ));
    }
}
