//! Game service configuration — every value the service needs that is not code.
//!
//! T-016 closed SEC-014 by moving the link-cipher key out of source. The legacy
//! hard-codes `0x12, 0x34` in `ITZSession::Init` (`ITZSession.cpp:56`) and that
//! literal is the only occurrence of the pair in the entire tree, so every
//! install, every session and every deployment share it and it never rotates
//! (`known_legacy_bugs.md` SEC-014). The shipped client cannot be changed — it
//! carries the constant in its own `.text` — so this port has to send the same
//! bytes to talk to it at all. What it must not do is *also* have them in its
//! own source, where a second copy silently becomes a second truth.
//!
//! So: [`GameConfig::from_env`] reads the key from the environment and **fails
//! to start** if it is absent. There is no default. The accepted run supplies it
//! from `scripts\start-m1.ps1`, which is configuration; the Rust source contains
//! no key material.
//!
//! Environment:
//!
//! | Variable | Required | Meaning |
//! |---|---|---|
//! | `GAME_LISTEN` | no | `ip:port` for the client link (default `0.0.0.0:4010`) |
//! | `DATABASE_URL` | no | PostgreSQL URL (default in [`GameConfig`]) |
//! | `GAME_LINK_KEY1` / `GAME_LINK_KEY2` | **yes** | link-cipher key bytes, hex |
//! | `GAME_CLIENT_COMPAT_VERSION` | no | `siVersion` in `ClientCharUnique` |
//! | `GAME_SERVICE_AREA` | no | `siServiceArea`; hex, `0x08` = English |
//! | `GAME_SERVER_UNIQUE` | no | `siServerUnique` |
//! | `GAME_DATA_ROOT` | no | client data tree the field-monster load reads (default `../New Era Live`) |
//! | `GAME_HANDSHAKE_TIMEOUT_MS` | no | override for the 10 s handshake window |
//! | `GAME_CONNECT_TIMEOUT_MS` | no | override for the 7 s outbound connect window |
//! | `GAME_CLOSE_THROTTLE_MS` | no | override for the 600 ms close throttle |
//! | `GAME_DEBUG_LOG` | no | `1`/`true`/`on` prints the debug-tier log lines (probes that repeat on a timer) |
//!
//! There is deliberately no `GAME_PROLOGUE` / `GAME_WORLD_BULK` any more. The
//! capture-replay mode they selected was retired on 2026-10-09: world entry is
//! generated from PostgreSQL and `char_unique` is allocated per session. Both
//! variables are now ignored rather than rejected, so a stale one left in a shell
//! cannot break a launch.

use std::path::PathBuf;
use std::time::Duration;

use gz_net::SessionTimeouts;

/// `DEFAULT_DATABASE_URL`, kept next to the other defaults so there is one place
/// to look for "what does this service assume".
///
/// Deliberately credential-free: no user or password, so no secret sits in
/// source. sqlx fills user/password from the `PGUSER`/`PGPASSWORD` libpq env
/// vars (`sqlx-postgres` 0.8 `options/parse.rs:10`); the launcher scripts
/// source a gitignored `scripts/dev-db.ps1`. Anything else sets `DATABASE_URL`.
pub const DEFAULT_DATABASE_URL: &str = "postgres://localhost:5432/goonzu";

/// The legacy game link's port. `GameProtocol.cpp:32` — the `CreateListen` call
/// there is commented out, but `:27`'s `CreateFactory` and the capture's `:4010`
/// agree, and the front service hands this port to the client.
const DEFAULT_LISTEN: &str = "0.0.0.0:4010";

/// `siVersion` in `cltGameMsgResponse_ClientCharUnique`. The legacy sends the
/// build's protocol version here; `1` is what the shipped `New Era.exe` answers
/// to, verified live on 2026-10-05.
const DEFAULT_CLIENT_COMPAT_VERSION: i16 = 1;

/// `SERVICEAREA_ENGLISH`, from `ServiceArea.txt`. This is the value that put the
/// login dialog inside `ConstSwitch_TextEncryption` — the 2026-10-05 RE's other
/// half.
const DEFAULT_SERVICE_AREA: i16 = 0x08;

const DEFAULT_SERVER_UNIQUE: i16 = 1;

/// The client data tree the field-monster load reads, relative to a `cargo run`
/// from `goonzu-rs` (`New Era Live\Map\MainMap\MainMap.map`, `NPCGroup\*.txt`,
/// `Data\KindInfo_Create.txt`, `Data\KindInfo_Unique.txt`). `GAME_DATA_ROOT`
/// overrides it; a missing tree degrades to an empty field, not a failed start.
const DEFAULT_DATA_ROOT: &str = "../New Era Live";

/// Everything the game service reads from outside its source.
#[derive(Debug, Clone)]
pub struct GameConfig {
    pub database_url: String,
    pub listen: String,
    /// Link-cipher key bytes — the second cipher instance, `GetEncrypt2()`.
    /// There is deliberately no `Default`: see the module docs.
    pub link_key: (u8, u8),
    pub client_compat_version: i16,
    pub service_area: i16,
    pub server_unique: i16,
    pub timeouts: SessionTimeouts,
    /// The client data tree the field-monster load reads. See
    /// [`DEFAULT_DATA_ROOT`].
    pub data_root: PathBuf,
    /// Whether the debug-tier log lines print — probes that answer an open
    /// question but repeat on a timer (e.g. the 230 heartbeat's type-`N` line).
    /// Off by default so the console stays readable during normal play.
    pub debug_log: bool,
}

impl GameConfig {
    /// Read the environment, or explain precisely what is missing.
    ///
    /// # Errors
    ///
    /// Returns a message naming the variable and the expected syntax when a
    /// value is absent or unparsable. Nothing is defaulted silently except the
    /// three documented above.
    pub fn from_env() -> Result<Self, String> {
        let database_url =
            std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_string());
        let listen = std::env::var("GAME_LISTEN").unwrap_or_else(|_| DEFAULT_LISTEN.to_string());

        // SEC-014: no default. The legacy literal is not repeated here; it lives
        // in scripts\start-m1.ps1, which is configuration.
        let link_key = (
            required_hex_u8("GAME_LINK_KEY1")?,
            required_hex_u8("GAME_LINK_KEY2")?,
        );

        let client_compat_version =
            optional_i16("GAME_CLIENT_COMPAT_VERSION", DEFAULT_CLIENT_COMPAT_VERSION)?;
        let service_area = optional_i16("GAME_SERVICE_AREA", DEFAULT_SERVICE_AREA)?;
        let server_unique = optional_i16("GAME_SERVER_UNIQUE", DEFAULT_SERVER_UNIQUE)?;

        let data_root = std::env::var_os("GAME_DATA_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_DATA_ROOT));

        let debug_log = std::env::var("GAME_DEBUG_LOG")
            .is_ok_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "on"));

        let defaults = SessionTimeouts::default();
        let timeouts = SessionTimeouts {
            handshake: optional_millis("GAME_HANDSHAKE_TIMEOUT_MS", defaults.handshake)?,
            connect: optional_millis("GAME_CONNECT_TIMEOUT_MS", defaults.connect)?,
            close_throttle: optional_millis("GAME_CLOSE_THROTTLE_MS", defaults.close_throttle)?,
        };

        Ok(Self {
            database_url,
            listen,
            link_key,
            client_compat_version,
            service_area,
            server_unique,
            timeouts,
            data_root,
            debug_log,
        })
    }

    /// The character id the client is told to key its per-character records on.
    ///
    /// Always the per-session allocation — the legacy's shape: `m_uiCharID` is a
    /// session-scoped id handed out by the session manager
    /// (`CharServer-Session.cpp:58` passes it straight into `MakeSimpleChar`), not a
    /// database column. The retired capture mode overrode this with the reference
    /// session's `0x05BF`, which is what made two-client testing impossible: two
    /// sessions shared one id, so each treated the other's frames as its own
    /// (`bselfswitch`, `Client-Person-BigInfo.cpp:1160-1176`).
    ///
    /// The durable per-character id lives in `characters.game_unique` and
    /// `handle_select_char` cross-checks the two.
    #[must_use]
    pub const fn char_unique(&self, allocated: u32) -> u32 {
        allocated
    }
}

fn required_hex_u8(name: &str) -> Result<u8, String> {
    let raw = std::env::var(name).map_err(|_| {
        format!(
            "{name} is required (SEC-014: link-cipher key material must come from \
             configuration, never a source default). Set it in scripts\\start-m1.ps1 \
             or your process environment. Expected one hex byte, e.g. {name}=12"
        )
    })?;
    let trimmed = raw.trim();
    let digits = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    u8::from_str_radix(digits, 16).map_err(|_| {
        format!("{name}={raw:?} is not one hex byte; expected 00..=ff, with or without 0x")
    })
}

fn optional_i16(name: &str, fallback: i16) -> Result<i16, String> {
    match std::env::var(name) {
        Ok(raw) => {
            let trimmed = raw.trim();
            let parsed = match trimmed
                .strip_prefix("0x")
                .or_else(|| trimmed.strip_prefix("0X"))
            {
                Some(digits) => i16::from_str_radix(digits, 16),
                None => trimmed.parse(),
            };
            parsed
                .map_err(|_| format!("{name}={raw:?} is not an i16; use decimal (1) or hex (0x08)"))
        }
        Err(_) => Ok(fallback),
    }
}

fn optional_millis(name: &str, fallback: Duration) -> Result<Duration, String> {
    match std::env::var(name) {
        Ok(raw) => {
            let ms: u64 = raw
                .trim()
                .parse()
                .map_err(|_| format!("{name}={raw:?} is not a whole number of milliseconds"))?;
            Ok(Duration::from_millis(ms))
        }
        Err(_) => Ok(fallback),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialises everything that touches the process environment, for the
    /// whole body of a test.
    ///
    /// The lock is held by the [`EnvGuard`], not by `take()`, deliberately: the
    /// vars stay set for the whole test, so a lock that only covered `take()`
    /// would let two tests interleave and each see the other's values.
    static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    const TOUCHED: [&str; 13] = [
        "DATABASE_URL",
        "GAME_LISTEN",
        "GAME_LINK_KEY1",
        "GAME_LINK_KEY2",
        "GAME_CLIENT_COMPAT_VERSION",
        "GAME_SERVICE_AREA",
        "GAME_SERVER_UNIQUE",
        "GAME_HANDSHAKE_TIMEOUT_MS",
        "GAME_CONNECT_TIMEOUT_MS",
        "GAME_CLOSE_THROTTLE_MS",
        "GAME_DEBUG_LOG",
        "GAME_PROLOGUE",
        "GAME_WORLD_BULK",
    ];

    /// Exclusive access to the environment for one test, restoring it after.
    ///
    /// `set_var`/`remove_var` are `unsafe` in edition 2024 — the environment is
    /// process-wide shared mutable state and concurrent access from another
    /// thread is UB, not a race the runtime absorbs. The safety argument here is
    /// that this test module is the *only* code in the `game` binary that reads
    /// or writes the environment, and every such thread must hold this one
    /// `static` lock for the whole of its body — which is why [`Self::set`] and
    /// [`Self::unset`] take `&self` and there is no free function that reaches
    /// `set_var`. Holding the guard for the duration makes the argument true by
    /// construction rather than by convention.
    struct EnvGuard {
        // Declared first so it is the last thing released: `Drop::drop` restores
        // the environment while the lock is still held.
        _lock: std::sync::MutexGuard<'static, ()>,
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn take() -> Self {
            let lock = ENV.lock().unwrap_or_else(|e| e.into_inner());
            let saved = TOUCHED
                .iter()
                .map(|k| (*k, std::env::var(k).ok()))
                .collect();
            Self { _lock: lock, saved }
        }

        fn set(&self, key: &str, value: &str) {
            // SAFETY: the caller holds `ENV` for its whole body, so no other
            // thread in this binary is in `set_var` — see the type's docs.
            unsafe { std::env::set_var(key, value) }
        }

        fn unset(&self, key: &str) {
            // SAFETY: as `set`.
            unsafe { std::env::remove_var(key) }
        }

        /// The minimal environment `from_env` accepts: just the link key.
        fn key_only(&self) {
            for key in TOUCHED {
                self.unset(key);
            }
            self.set("GAME_LINK_KEY1", "12");
            self.set("GAME_LINK_KEY2", "34");
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in &self.saved {
                match value {
                    Some(v) => self.set(key, v),
                    None => self.unset(key),
                }
            }
        }
    }

    #[test]
    fn the_link_key_is_required_and_names_itself_when_missing() {
        let env = EnvGuard::take();
        env.key_only();
        env.unset("GAME_LINK_KEY1");
        let err = GameConfig::from_env().unwrap_err();
        assert!(err.contains("GAME_LINK_KEY1"), "{err}");
        assert!(err.contains("SEC-014"), "{err}");
    }

    #[test]
    fn the_link_key_is_required_in_both_positions() {
        let env = EnvGuard::take();
        env.key_only();
        env.unset("GAME_LINK_KEY2");
        let err = GameConfig::from_env().unwrap_err();
        assert!(err.contains("GAME_LINK_KEY2"), "{err}");
    }

    #[test]
    fn the_link_key_accepts_hex_with_or_without_a_prefix() {
        let env = EnvGuard::take();
        // (as written, as expected to parse) — the last two cases are about
        // trimming, so they carry their own expected bytes instead of reusing
        // 0x12/0x34.
        for (a, b, want) in [
            ("12", "34", (0x12u8, 0x34u8)),
            ("0x12", "0x34", (0x12, 0x34)),
            ("0X12", "34", (0x12, 0x34)),
            (" ff ", "0", (0xff, 0x00)),
            ("\t0x0A\n", "0xFF", (0x0a, 0xff)),
        ] {
            env.set("GAME_LINK_KEY1", a);
            env.set("GAME_LINK_KEY2", b);
            let cfg = GameConfig::from_env().unwrap_or_else(|e| panic!("{a:?},{b:?}: {e}"));
            assert_eq!(cfg.link_key, want, "{a:?},{b:?}");
        }
    }

    #[test]
    fn a_malformed_link_key_names_the_variable_and_the_range() {
        let env = EnvGuard::take();
        env.key_only();
        env.set("GAME_LINK_KEY1", "zz");
        let err = GameConfig::from_env().unwrap_err();
        assert!(err.contains("GAME_LINK_KEY1"), "{err}");
        assert!(err.contains("00..=ff"), "{err}");

        // Two bytes in one variable is the likely mistake, not a missing one.
        env.set("GAME_LINK_KEY1", "1234");
        let err = GameConfig::from_env().unwrap_err();
        assert!(err.contains("GAME_LINK_KEY1"), "{err}");
    }

    #[test]
    fn the_documented_defaults_are_the_legacy_values() {
        let env = EnvGuard::take();
        env.key_only();
        let cfg = GameConfig::from_env().unwrap();
        assert_eq!(cfg.listen, "0.0.0.0:4010");
        assert_eq!(cfg.database_url, DEFAULT_DATABASE_URL);
        assert_eq!(cfg.client_compat_version, 1);
        assert_eq!(cfg.service_area, 0x08);
        assert_eq!(cfg.server_unique, 1);
    }

    /// A committed DB password in the fallback URL is how the credential leaked
    /// into source. The default must stay credential-free; `DATABASE_URL` or
    /// `PGPASSWORD`/`PGUSER` (read from the process env by sqlx, like libpq)
    /// carry it at run time instead.
    #[test]
    fn the_database_default_is_credential_free() {
        assert_eq!(DEFAULT_DATABASE_URL, "postgres://localhost:5432/goonzu");
    }

    /// The replay mode is gone, and with it the two knobs that selected it.
    ///
    /// `GAME_PROLOGUE` and `GAME_WORLD_BULK` are no longer read. They are asserted
    /// *ignored* rather than merely absent, because a stale `GAME_PROLOGUE=capture`
    /// still sitting in a shell or a launch script must not be able to reintroduce
    /// the retired mode or fail startup: the server generates everything from
    /// PostgreSQL and there is nothing left to select.
    #[test]
    fn the_retired_mode_knobs_are_no_longer_read() {
        let env = EnvGuard::take();
        env.key_only();
        env.set("GAME_PROLOGUE", "capture");
        env.set("GAME_WORLD_BULK", "on230");
        // Reads the environment, so if either variable were consulted again it
        // would either fail to parse or take effect.
        let cfg = GameConfig::from_env().unwrap();
        assert_eq!(cfg.listen, "0.0.0.0:4010");
        assert_eq!(cfg.service_area, 0x08);
    }

    /// Every session gets its own character id, always.
    ///
    /// This is the property the retired capture mode broke: pinning every session
    /// to one id made two clients share a character, so each treated the other's
    /// frames as its own (`bselfswitch`, `Client-Person-BigInfo.cpp:1160-1176`) and
    /// wrote the other's life/mana into its own avatar (`:1228-1238`, unconditional).
    #[test]
    fn char_unique_is_always_the_per_session_allocation() {
        let env = EnvGuard::take();
        env.key_only();
        let cfg = GameConfig::from_env().unwrap();
        assert_eq!(cfg.char_unique(4_711), 4_711);
        assert_eq!(cfg.char_unique(4_712), 4_712);
        assert_ne!(
            cfg.char_unique(4_711),
            cfg.char_unique(4_712),
            "two sessions must never share a character id"
        );
        assert_ne!(
            cfg.char_unique(4_711),
            gz_proto::captured_lengths::CAPTURED_CHAR_UNIQUE as u32,
            "and neither may be the retired capture's id"
        );
    }

    #[test]
    fn timeouts_default_to_the_three_legacy_literals() {
        let env = EnvGuard::take();
        env.key_only();
        let cfg = GameConfig::from_env().unwrap();
        assert_eq!(cfg.timeouts, SessionTimeouts::default());
        assert_eq!(cfg.timeouts.handshake, Duration::from_millis(10_000));
        assert_eq!(cfg.timeouts.connect, Duration::from_millis(7_000));
        assert_eq!(cfg.timeouts.close_throttle, Duration::from_millis(600));
    }

    #[test]
    fn every_timeout_is_overridable() {
        let env = EnvGuard::take();
        env.key_only();
        env.set("GAME_HANDSHAKE_TIMEOUT_MS", "250");
        env.set("GAME_CONNECT_TIMEOUT_MS", "1500");
        env.set("GAME_CLOSE_THROTTLE_MS", "10");
        let cfg = GameConfig::from_env().unwrap();
        assert_eq!(cfg.timeouts.handshake, Duration::from_millis(250));
        assert_eq!(cfg.timeouts.connect, Duration::from_millis(1500));
        assert_eq!(cfg.timeouts.close_throttle, Duration::from_millis(10));

        env.set("GAME_HANDSHAKE_TIMEOUT_MS", "soon");
        let err = GameConfig::from_env().unwrap_err();
        assert!(err.contains("GAME_HANDSHAKE_TIMEOUT_MS"), "{err}");
    }

    /// The debug tier is off unless asked for, and accepts the documented
    /// spellings. A probe line like the 230 heartbeat's type-`N` line repeats
    /// on a timer, so its default has to be silence.
    #[test]
    fn debug_log_is_off_unless_enabled() {
        let env = EnvGuard::take();
        env.key_only();
        assert!(!GameConfig::from_env().unwrap().debug_log);

        for on in ["1", "true", "on", " TRUE "] {
            env.set("GAME_DEBUG_LOG", on);
            assert!(
                GameConfig::from_env().unwrap().debug_log,
                "{on:?} should enable"
            );
        }
        for off in ["0", "false", ""] {
            env.set("GAME_DEBUG_LOG", off);
            assert!(
                !GameConfig::from_env().unwrap().debug_log,
                "{off:?} should not enable"
            );
        }
    }

    #[test]
    fn service_area_and_version_take_decimal_or_hex() {
        let env = EnvGuard::take();
        env.key_only();
        env.set("GAME_SERVICE_AREA", "0x02");
        env.set("GAME_CLIENT_COMPAT_VERSION", "7");
        let cfg = GameConfig::from_env().unwrap();
        assert_eq!(cfg.service_area, 2);
        assert_eq!(cfg.client_compat_version, 7);

        env.set("GAME_SERVICE_AREA", "Korea");
        let err = GameConfig::from_env().unwrap_err();
        assert!(err.contains("GAME_SERVICE_AREA"), "{err}");
    }

    /// The guard's whole reason: two tests that both set the same variable would
    /// otherwise race, and the failure mode is a test that passes for the wrong
    /// reason rather than a flake that announces itself.
    ///
    /// Asserted with `try_lock` rather than by spawning a second thread and
    /// joining it. A thread calling `EnvGuard::take()` here would block on the
    /// lock this test holds — which is the behaviour under test, but it means
    /// `join()` never returns and the test *hangs* instead of passing. A test
    /// that can hang is not a test.
    #[test]
    fn the_guard_holds_the_lock_for_the_whole_body() {
        let env = EnvGuard::take();

        assert!(
            ENV.try_lock().is_err(),
            "the guard released the lock at take() instead of on drop, so two env \
             tests can interleave and see each other's variables"
        );

        // Still exclusive, and still mutable: the body is the guarded region.
        env.key_only();
        env.set("GAME_SERVICE_AREA", "0x02");
        assert_eq!(GameConfig::from_env().unwrap().service_area, 2);
    }

    #[test]
    fn dropping_the_guard_restores_the_environment_and_releases_the_lock() {
        fn snapshot() -> Vec<(&'static str, Option<String>)> {
            TOUCHED
                .iter()
                .map(|k| (*k, std::env::var(k).ok()))
                .collect()
        }

        let env = EnvGuard::take();
        let before = snapshot();

        // Wreck it: `key_only` clears every touched variable and then sets two.
        env.key_only();
        env.set("GAME_SERVICE_AREA", "0x02");
        env.set("GAME_LISTEN", "10.0.0.1:9");
        assert_ne!(before, snapshot(), "sanity: the body must change something");

        drop(env);

        // Deliberately *not* `ENV.try_lock()` here. Between the drop and the
        // check any other test in this binary may legitimately hold the lock, so
        // a post-drop assertion is a race — and it fails for the right reason
        // and the wrong one. Re-acquiring is both the release test and the
        // restore test: it blocks until every in-flight guard has finished and
        // restored, and then `saved` is what the environment actually held.
        let next = EnvGuard::take();
        assert_eq!(
            before, next.saved,
            "the first guard did not restore the environment it found"
        );
        assert_eq!(
            before,
            snapshot(),
            "and the environment drifted on top of it"
        );
    }
}
