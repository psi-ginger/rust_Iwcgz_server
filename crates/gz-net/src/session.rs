//! The `ITZSession` lifecycle: the four states, the three legacy timings, and
//! the rule that a refusal is a disconnect (task T-016).
//!
//! Legacy source: `NetworkLib\ITZNetLib\ITZSession.{h,cpp}`, `ITZNetIOCP.cpp`,
//! `ITZListen.cpp`, `ITZObject.h`. What is *not* here is the IOCP itself — the
//! legacy's overlapped `WSARecv`/`WSASend` ring, its 20 ms worker thread, and
//! the pooled session objects all exist to turn a blocking socket into an event
//! loop. A port that gives each connection its own task needs none of it, so
//! this module is the *policy* half: what the state may be, when it must
//! change, and when the port gives up on it. It is synchronous and
//! dependency-free, like the rest of the crate.
//!
//! # The three timings
//!
//! | Constant | Legacy site | What it actually guards |
//! |---|---|---|
//! | [`HANDSHAKE_TIMEOUT`] | `ITZNetIOCP.cpp:41-49` | accept → first inbound frame |
//! | [`CONNECT_TIMEOUT`] | `ITZSession.cpp:238-243` | an **outbound** `connect()` that has not completed |
//! | [`CLOSE_THROTTLE`] | `ITZSession.cpp:213-215` | how soon a session object may be **re-dialed** after a close |
//!
//! Only the first applies to a listener. The other two are properties of
//! `ITZSession::_connect`, the outbound dial path, and the front and game
//! services in this port only ever `accept` — so
//! [`Session::may_connect`] and [`Session::connect_expired`] have no caller
//! there yet. They are modelled and tested here because the first link that
//! *does* dial out (a front→game hop, a supervisor health probe) must not
//! rediscover the numbers, and because [`Session`] is the crate's only place
//! that holds them.
//!
//! # Deliberate divergence: 10 s is a real handshake deadline here
//!
//! The legacy's 10 s window is **not** a handshake deadline. It tests
//!
//! ```cpp
//! if( pSession->IsActive() && pSession->GetState() != SESSION_STATE_ESTABLISHED )
//! {
//!     if( tick - pSession->m_dwEstablishedTick >= 10 * 1000 ) { /* kill */ }
//! }
//! ```
//!
//! (`ITZNetIOCP.cpp:41-49`) against `m_dwEstablishedTick`, which `SetSocket`
//! stamps at accept (`ITZSession.cpp:165-167`). But `_beginSession` promotes
//! `CONNECTED` → `ESTABLISHED` on the worker's very next pass — it posts the
//! first `WSARecv` and immediately calls `SetState(SESSION_STATE_ESTABLISHED)`,
//! `ITZNetIOCP.cpp:52-63` — without waiting for a single byte from the peer. The
//! worker sleeps 20 ms (`ITZNetIOCP.cpp:23`), so the window shuts after ~20 ms
//! in practice and the watchdog can only ever fire on a session stuck in `NONE`
//! (a `SetSocket` that failed) or a worker that has stopped running. The client
//! is not, and cannot be, on the hook: a peer that connects and then says
//! nothing stays `ESTABLISHED` indefinitely, and the only liveness control on
//! the game link is `cltCharServer::AutoLogOut`'s three-minute idle check
//! (`char.cpp:5140-5149`) — which needs a prior receive to arm, because
//! `LastReceveFromClientClock == 0` short-circuits it (`char.cpp:5145`).
//!
//! A port with one task per socket has no worker tick to inherit that accident
//! from, so [`Session::establish`] is called on the **first inbound frame** and
//! [`Session::handshake_expired`] measures accept → that frame, which is what
//! "10 s handshake timeout" is normally taken to mean and what actually
//! reclaims a silent half-open connection. The number is the legacy's; the
//! window is the fix.
//!
//! # `IsActive()` is not the session state
//!
//! The watchdog's other predicate is `ITZObject::IsActive()`, the list flag
//! flipped by `ITZList::push`/`pop` (`ITZObject.h:20`, `ITZList.h:174,251`),
//! which tracks whether the session sits in the master list — not how far
//! through the handshake it is. Keeping the two apart matters: a session that
//! has been popped is not on the watchdog's walk at all, so it is neither
//! watched nor timed out. [`Session::set_active`] reproduces the distinction
//! rather than folding it into [`SessionState`].

use crate::{FrameError, SessionState};
use std::time::Duration;

/// `tick - m_dwEstablishedTick >= 10 * 1000`. `ITZNetIOCP.cpp:43`.
///
/// See the module docs for why the window this port measures is not the one the
/// legacy measures.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_millis(10_000);

/// `GetTickCount() - m_dwConnectTick >= 7000`. `ITZSession.cpp:238`.
pub const CONNECT_TIMEOUT: Duration = Duration::from_millis(7_000);

/// `abs( (int)(GetTickCount() - m_dwCloseTick) ) < 600`. `ITZSession.cpp:213`.
pub const CLOSE_THROTTLE: Duration = Duration::from_millis(600);

/// A millisecond clock reading, standing in for `GetTickCount()`.
///
/// The legacy's is a `DWORD` that wraps every 49.7 days. [`Tick`] is a `u64` so
/// this port does not have to reproduce that, and every subtraction below
/// saturates rather than wrapping — see [`Session::may_connect`] for the one
/// place where that changes an answer instead of just widening it.
pub type Tick = u64;

/// The legacy's three hard-coded timings in one place, overridable per link.
///
/// [`SessionTimeouts::default`] is the legacy's literal set. A deployment that
/// wants different numbers sets them here rather than at each call site, which
/// is the only knob this port adds over the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionTimeouts {
    /// Accept → first inbound frame.
    pub handshake: Duration,
    /// Outbound `connect()` → give up.
    pub connect: Duration,
    /// Close → earliest re-dial of the same session object.
    pub close_throttle: Duration,
}

impl Default for SessionTimeouts {
    fn default() -> Self {
        Self {
            handshake: HANDSHAKE_TIMEOUT,
            connect: CONNECT_TIMEOUT,
            close_throttle: CLOSE_THROTTLE,
        }
    }
}

/// Why a session was refused, and therefore closed.
///
/// The legacy has no such enum: every refusal funnels into `CloseSocket();`
/// then `return` and the reason is lost (`GameProtocol.cpp:94`, `:135`). The
/// distinction this port keeps is the one an operator needs — a peer that broke
/// the wire is not a peer that failed to authenticate, and neither should
/// quietly become the other in a log line.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Rejection {
    /// No inbound frame inside [`SessionTimeouts::handshake`].
    HandshakeTimeout,
    /// The frame header declared an impossible size. See [`FrameError`].
    Framing(FrameError),
    /// The link cipher's integrity byte did not match.
    ///
    /// BUG-012/BUG-013 territory: the only reason a session is dropped over a
    /// frame that parsed cleanly.
    Integrity,
    /// A frame arrived before the protocol expected one, or in the wrong state.
    OutOfOrder {
        /// What the link expected instead.
        expected: &'static str,
    },
    /// An authenticated identity the session state cannot carry forward.
    ///
    /// Legacy shape: `AuthProcess.cpp` sends a failure response and returns,
    /// and the front link's own socket stays open (proved in the 2026-10-05 RE,
    /// `AuthProcess.cpp:134` — `CloseSocket()` fires only on a size mismatch).
    /// On the game link the equivalent is a login the session refuses.
    Denied {
        /// Opcode the refusal is about.
        cmd: u16,
        /// Human-readable cause. Never carries credentials.
        reason: String,
    },
}

impl Rejection {
    /// Whether the legacy would have left the socket open to deliver a reply.
    ///
    /// Only [`Rejection::Denied`] ever does, and only on the front link, where
    /// the response body carries the dialog text. Everything else is a wire
    /// failure with nothing to say, so the port drops the socket and the peer
    /// sees EOF.
    #[must_use]
    pub fn carries_reply(self) -> bool {
        matches!(self, Self::Denied { .. })
    }
}

/// One connection's session state, mirroring `ITZSession`'s fields.
///
/// In order: `m_siState` ([`Session::state`]), `m_dwEstablishedTick`
/// ([`Session::since`]), `m_dwCloseTick` ([`Session::last_close`]),
/// `m_dwConnectTick` + `m_bAttempting` ([`Session::connect_expired`],
/// [`Session::is_attempting`]), `m_socket` validity ([`Session::has_socket`]),
/// and `m_bActive` ([`Session::is_active`]).
///
/// The type is deliberately constructible and steppable with no I/O: a caller
/// drives it from whatever socket abstraction it holds, which is what keeps this
/// crate testable without a network.
#[derive(Debug, Clone)]
pub struct Session {
    timeouts: SessionTimeouts,
    state: SessionState,
    /// `m_dwEstablishedTick`. Stamped by [`Session::accept`] — not by
    /// `establish` — because the legacy stamps it inside `SetSocket` before the
    /// state moves (`ITZSession.cpp:167` before `:184`).
    since: Tick,
    /// `m_dwCloseTick`. `None` until a close actually happened; see
    /// [`Session::may_connect`] for why that is not `Some(0)`.
    last_close: Option<Tick>,
    /// `m_dwConnectTick`, meaningful only while `m_bAttempting`.
    connect_tick: Option<Tick>,
    /// `m_bAttempting`.
    attempting: bool,
    /// `m_socket != INVALID_SOCKET`, the guard `_closesocket` opens with
    /// (`ITZSession.cpp:278`).
    has_socket: bool,
    /// `m_bActive`, the list flag — *not* the handshake stage.
    active: bool,
}

impl Session {
    /// A fresh session in [`SessionState::None`] with the legacy timings.
    #[must_use]
    pub fn new(timeouts: SessionTimeouts) -> Self {
        Self {
            timeouts,
            state: SessionState::None,
            since: 0,
            last_close: None,
            connect_tick: None,
            attempting: false,
            has_socket: false,
            active: false,
        }
    }

    /// The timings this session enforces.
    #[must_use]
    pub const fn timeouts(&self) -> &SessionTimeouts {
        &self.timeouts
    }

    /// Current state.
    #[must_use]
    pub const fn state(&self) -> SessionState {
        self.state
    }

    /// The tick [`Session::accept`] stamped.
    #[must_use]
    pub const fn since(&self) -> Tick {
        self.since
    }

    /// `m_bActive` — in the master list, not "handshaked".
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.active
    }

    /// Set `m_bActive`. `ITZList::push`/`pop` (`ITZList.h:174,251`).
    pub fn set_active(&mut self, active: bool) {
        self.active = active;
    }

    /// Whether a socket is attached — the `_closesocket` guard, `m_bAttempting`
    /// excluded.
    #[must_use]
    pub const fn has_socket(&self) -> bool {
        self.has_socket
    }

    /// `m_bAttempting`. `ITZSession.cpp:228-236`.
    #[must_use]
    pub const fn is_attempting(&self) -> bool {
        self.attempting
    }

    /// The close stamp, for a caller that wants to log it.
    #[must_use]
    pub const fn last_close(&self) -> Option<Tick> {
        self.last_close
    }

    /// `SetSocket`: adopt an accepted socket, `NONE` → `CONNECTED`.
    ///
    /// Stamps [`Session::since`] *first*, exactly as the legacy does, and only
    /// then checks the state — so a refused `SetSocket` still moves
    /// `m_dwEstablishedTick` while changing nothing else
    /// (`ITZSession.cpp:165-171`). Returns whether the socket was adopted.
    pub fn accept(&mut self, now: Tick) -> bool {
        self.since = now;
        if self.state != SessionState::None {
            return false;
        }
        self.has_socket = true;
        self.set_state(SessionState::Connected);
        true
    }

    /// The `_beginSession` state change: `CONNECTED` → `ESTABLISHED`.
    ///
    /// The legacy reaches `ESTABLISHED` the instant it posts its first
    /// `WSARecv`, before any byte arrives (`ITZNetIOCP.cpp:52-63`). A port that
    /// owns the socket has no post to wait for, so it calls this on the first
    /// inbound frame instead — see the module docs. Either way it is one-way: a
    /// session that is not [`SessionState::Connected`] is left alone, so a late
    /// duplicate cannot bounce an established session backwards.
    pub fn establish(&mut self) {
        if self.state == SessionState::Connected {
            self.set_state(SessionState::Established);
        }
    }

    /// `_closesocket`: drop the socket, stamp the throttle, `*` → `DISCONNECTED`.
    ///
    /// Returns whether there was a socket to close. The legacy returns `FALSE`
    /// and — crucially — **does not** stamp `m_dwCloseTick` in that case,
    /// because that assignment sits inside the `m_socket != INVALID_SOCKET`
    /// block (`ITZSession.cpp:278`, `:308`, `:316`). So closing an
    /// already-dead socket neither starts nor extends the throttle.
    pub fn close(&mut self, now: Tick) -> bool {
        if !self.has_socket {
            return false;
        }
        self.has_socket = false;
        self.last_close = Some(now);
        self.attempting = false;
        self.connect_tick = None;
        self.set_state(SessionState::Disconnected);
        true
    }

    /// `SetState`, including its guard at `ITZSession.cpp:150-153`.
    ///
    /// `DISCONNECTED` requested while the session is already `NONE` is dropped.
    /// `_closesocket` pre-checks the same condition (`ITZSession.cpp:310-311`),
    /// so the guard is redundant on that path; it exists for callers that do not
    /// pre-check, and it is reproduced because a pooled session object reused
    /// after `DelSession` depends on not being resurrected this way.
    fn set_state(&mut self, state: SessionState) {
        if state == SessionState::Disconnected && self.state == SessionState::None {
            return;
        }
        self.state = state;
    }

    /// `ITZNetIOCP.cpp:41-49`: `active && state != ESTABLISHED && elapsed >= 10 s`.
    ///
    /// Returns the [`Rejection`] to close with, or `None` while the session
    /// still deserves to live.
    #[must_use]
    pub fn handshake_expired(&self, now: Tick) -> Option<Rejection> {
        if !self.active || self.state.is_active() {
            return None;
        }
        (now.saturating_sub(self.since) >= self.timeouts.handshake.as_millis() as u64)
            .then_some(Rejection::HandshakeTimeout)
    }

    /// Whether a session object may be re-dialed yet — the 600 ms close throttle.
    ///
    /// `abs( (int)(GetTickCount() - m_dwCloseTick) ) < 600` returns early
    /// (`ITZSession.cpp:213-215`). Two things about that expression are worth
    /// naming, because both are bugs and neither is portable:
    ///
    /// 1. The subtraction is `DWORD` arithmetic and so wraps; the result is then
    ///    reinterpreted as `SI32` and `abs`'d. Once the true delta exceeds
    ///    ~24.9 days — or `GetTickCount()` itself wraps at 49.7 — the cast turns
    ///    a large positive delta negative, `abs` hands back a huge number, and
    ///    the throttle silently stops applying so the session re-dials at once.
    /// 2. `m_dwCloseTick` starts at `0` (`ITZSession.cpp:11`), so on a machine
    ///    with under 600 ms of uptime the *first* `connect()` of the process is
    ///    refused. The throttle measures uptime, not the last close.
    ///
    /// This port reads "never closed" as "no throttle" — which is what (2)
    /// almost certainly meant — and compares a saturating `u64` delta, which
    /// cannot reproduce (1).
    #[must_use]
    pub fn may_connect(&self, now: Tick) -> bool {
        match self.last_close {
            None => true,
            Some(then) => {
                now.saturating_sub(then) >= self.timeouts.close_throttle.as_millis() as u64
            }
        }
    }

    /// Stamp `m_dwConnectTick` and set `m_bAttempting`, unless refused.
    ///
    /// The legacy stamps mid-dial and only on the first attempt
    /// (`ITZSession.cpp:228-236`); the two refusal tests it applies first are
    /// the throttle (`:213`) and the 7 s deadline (`:238`). Returns whether the
    /// attempt is in flight.
    pub fn begin_connect(&mut self, now: Tick) -> bool {
        if !self.may_connect(now) || self.connect_expired(now) {
            return false;
        }
        if !self.attempting {
            self.connect_tick = Some(now);
            self.attempting = true;
        }
        true
    }

    /// `GetTickCount() - m_dwConnectTick >= 7000` (`ITZSession.cpp:238-243`).
    ///
    /// False when no attempt is in flight: the legacy's check sits *after* the
    /// `m_bAttempting` stamp (`:228` then `:238`), so it is only reachable with
    /// one.
    #[must_use]
    pub fn connect_expired(&self, now: Tick) -> bool {
        match self.connect_tick {
            Some(then) if self.attempting => {
                now.saturating_sub(then) >= self.timeouts.connect.as_millis() as u64
            }
            _ => false,
        }
    }

    /// `connect()` returned: clear `m_bAttempting` (`ITZSession.cpp:266`).
    pub fn connect_settled(&mut self) {
        self.attempting = false;
        self.connect_tick = None;
    }

    /// Close the session because of `reason`, and mark it inactive.
    ///
    /// One code path for every refusal, so "a rejection is a disconnect, never
    /// a resynchronise" is structural here rather than a convention each call
    /// site has to remember. The [`Rejection`] is returned so the caller can
    /// log it — including its [`Rejection::carries_reply`] verdict — before the
    /// socket goes away.
    pub fn reject(&mut self, reason: Rejection, now: Tick) -> Rejection {
        self.close(now);
        self.set_state(SessionState::Disconnected);
        self.active = false;
        reason
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new(SessionTimeouts::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64) -> Tick {
        ms
    }

    /// `accept` + `set_active`: what `ITZListen`'s accept path leaves behind.
    fn connected(at_tick: Tick) -> Session {
        let mut s = Session::default();
        assert!(s.accept(at_tick));
        s.set_active(true);
        assert!(s.has_socket());
        s
    }

    #[test]
    fn the_three_timings_are_the_legacy_literals() {
        let t = SessionTimeouts::default();
        assert_eq!(t.handshake, Duration::from_millis(10_000)); // ITZNetIOCP.cpp:43
        assert_eq!(t.connect, Duration::from_millis(7_000)); // ITZSession.cpp:238
        assert_eq!(t.close_throttle, Duration::from_millis(600)); // ITZSession.cpp:213
    }

    #[test]
    fn a_new_session_is_none_and_dialable() {
        let s = Session::default();
        assert_eq!(s.state(), SessionState::None);
        assert!(!s.has_socket());
        assert!(!s.is_active());
        assert!(s.may_connect(at(0)));
        assert_eq!(s.handshake_expired(at(1_000_000)), None);
    }

    #[test]
    fn accept_is_only_legal_from_none() {
        // SetSocket's `if( m_siState == SESSION_STATE_NONE )` guard, plus the
        // ordering detail that the tick is stamped before the guard runs.
        let mut s = Session::default();
        assert!(s.accept(at(500)));
        assert_eq!(s.state(), SessionState::Connected);
        assert_eq!(s.since(), at(500));

        assert!(!s.accept(at(900)));
        assert_eq!(s.state(), SessionState::Connected);
        // ... and m_dwEstablishedTick moved anyway. ITZSession.cpp:165-171.
        assert_eq!(s.since(), at(900));
    }

    #[test]
    fn establish_is_one_way_from_connected() {
        let mut s = connected(at(0));
        s.establish();
        assert_eq!(s.state(), SessionState::Established);

        // A duplicate first-frame signal must not bounce an established session
        // backwards.
        s.establish();
        assert_eq!(s.state(), SessionState::Established);

        // And a session that was never accepted cannot skip ahead.
        let mut cold = Session::default();
        cold.establish();
        assert_eq!(cold.state(), SessionState::None);
    }

    #[test]
    fn the_handshake_deadline_lands_on_the_tenth_second() {
        let s = connected(at(1_000));
        assert_eq!(s.state(), SessionState::Connected);
        assert_eq!(s.handshake_expired(at(10_999)), None);
        assert_eq!(
            s.handshake_expired(at(11_000)),
            Some(Rejection::HandshakeTimeout)
        );
        assert_eq!(
            s.handshake_expired(at(999_999)),
            Some(Rejection::HandshakeTimeout)
        );
    }

    #[test]
    fn the_handshake_deadline_survives_a_clock_that_went_backwards() {
        // GetTickCount is monotonic; a saturating delta means a bad reading
        // cannot underflow into a spurious timeout.
        let s = connected(at(5_000));
        assert_eq!(s.handshake_expired(at(0)), None);
    }

    #[test]
    fn an_established_session_is_never_handshake_expired() {
        let mut s = connected(at(0));
        s.establish();
        assert_eq!(s.handshake_expired(at(999_999)), None);
    }

    #[test]
    fn an_inactive_session_is_never_handshake_expired() {
        // The predicate is `IsActive() && ...`, and IsActive() is the list flag,
        // not the handshake stage -- a popped session is off the walk entirely.
        let mut s = Session::default();
        assert!(s.accept(at(0)));
        assert!(!s.is_active());
        assert_eq!(s.handshake_expired(at(600_000)), None);

        s.set_active(true);
        assert_eq!(
            s.handshake_expired(at(600_000)),
            Some(Rejection::HandshakeTimeout)
        );
    }

    #[test]
    fn close_stamps_the_throttle_once() {
        let mut s = connected(at(0));
        assert!(s.close(at(5_000)));
        assert_eq!(s.state(), SessionState::Disconnected);
        assert_eq!(s.last_close(), Some(at(5_000)));
        assert!(!s.may_connect(at(5_599)));
        assert!(s.may_connect(at(5_600)));

        // A second close finds no socket. ITZSession.cpp:278/316 returns early,
        // and m_dwCloseTick sits inside that block (:308), so the throttle
        // cannot be pushed out by a redundant close.
        assert!(!s.close(at(5_100)));
        assert_eq!(s.last_close(), Some(at(5_000)));
        assert!(!s.may_connect(at(5_599)));
    }

    #[test]
    fn close_from_none_is_a_no_op() {
        // No socket was ever attached, so _closesocket returns FALSE and
        // SetState's DISCONNECTED-from-NONE guard (:150-153) drops the rest.
        let mut s = Session::default();
        assert!(!s.close(at(1)));
        assert_eq!(s.state(), SessionState::None);
        assert_eq!(s.last_close(), None);
    }

    #[test]
    fn a_session_that_never_closed_has_no_throttle() {
        // m_dwCloseTick starts at 0 in the legacy, so a process with under 600 ms
        // of uptime refuses its first connect. This port reads "never closed" as
        // "no throttle".
        let s = Session::default();
        assert!(s.may_connect(at(0)));
        assert!(s.may_connect(at(599)));
    }

    #[test]
    fn the_connect_deadline_is_only_live_while_attempting() {
        let mut s = Session::default();
        assert!(s.begin_connect(at(0)));
        assert!(s.is_attempting());
        assert!(!s.connect_expired(at(6_999)));
        assert!(s.connect_expired(at(7_000)));

        // The legacy closes the attempt on expiry (_closesocket(); goto end;
        // ITZSession.cpp:240-241), which is this.
        s.connect_settled();
        assert!(!s.is_attempting());
        assert!(!s.connect_expired(at(999_999)));
    }

    #[test]
    fn a_throttled_session_cannot_begin_a_connect() {
        let mut s = connected(at(0));
        assert!(s.close(at(1_000)));
        assert!(!s.begin_connect(at(1_599)));
        assert!(s.begin_connect(at(1_600)));
    }

    #[test]
    fn begin_connect_does_not_restamp_a_running_attempt() {
        // `if( m_bAttempting == FALSE )` (ITZSession.cpp:228) -- the second poll
        // must not push the 7 s deadline out from under the first.
        let mut s = Session::default();
        assert!(s.begin_connect(at(0)));
        assert!(s.begin_connect(at(1_000)));
        assert!(s.connect_expired(at(7_500)));
    }

    #[test]
    fn every_rejection_ends_in_disconnect() {
        let reasons = [
            Rejection::HandshakeTimeout,
            Rejection::Framing(FrameError::SizeBelowMinimum { declared: 0 }),
            Rejection::Integrity,
            Rejection::OutOfOrder {
                expected: "the login request",
            },
            Rejection::Denied {
                cmd: 2,
                reason: "auth key 17 does not match account 17".to_string(),
            },
        ];
        for reason in reasons {
            let mut s = connected(at(0));
            s.establish();
            assert!(s.state().is_active());
            assert_eq!(s.reject(reason.clone(), at(2_000)), reason);
            assert_eq!(s.state(), SessionState::Disconnected);
            assert!(!s.is_active());
            assert!(!s.has_socket());
            assert!(!s.state().is_active());
        }
    }

    #[test]
    fn only_denials_carry_a_reply() {
        assert!(!Rejection::HandshakeTimeout.carries_reply());
        assert!(!Rejection::Integrity.carries_reply());
        assert!(!Rejection::OutOfOrder { expected: "" }.carries_reply());
        assert!(
            Rejection::Denied {
                cmd: 2,
                reason: String::new()
            }
            .carries_reply()
        );
    }

    #[test]
    fn timeouts_are_overridable_without_touching_call_sites() {
        let quick = SessionTimeouts {
            handshake: Duration::from_millis(250),
            ..SessionTimeouts::default()
        };
        let mut s = Session::new(quick);
        assert!(s.accept(at(0)));
        s.set_active(true);
        assert_eq!(s.handshake_expired(at(249)), None);
        assert_eq!(
            s.handshake_expired(at(250)),
            Some(Rejection::HandshakeTimeout)
        );
        // The other two keep the legacy numbers.
        assert_eq!(s.timeouts().connect, CONNECT_TIMEOUT);
        assert_eq!(s.timeouts().close_throttle, CLOSE_THROTTLE);
    }
}
