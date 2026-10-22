//! The PiFmRds renderer: pipe the sender's audio into a Raspberry Pi FM transmitter.
//!
//! Rust counterpart of `macast/plugins/renderer/pi_fm.py`'s decision layer (`:37-90`).
//!
//! ## The four upstream defects this module's shape answers
//!
//! The plugin's own header lists them, and three are about **when you are allowed to
//! spawn**:
//!
//! * `__init__` spawned `sudo pi_fm_rds` unconditionally. On a machine without the binary —
//!   or where `sudo` waits for a password on a thread nobody is watching — that **hung the
//!   app**. So the binary checks happen before anything is spawned, and a missing program is
//!   an error with a sentence, not a hang.
//! * `stop()` called `os._exit()`, a `TypeError` with no argument: it could never have
//!   worked, and had it worked it would have killed Macast instead of stopping the renderer.
//!   Nothing here can end the process.
//! * `os.waitpid(-1, 1)` in `set_media_stop` could reap **an unrelated child**. So the two
//!   children are held by handle and only those handles are signalled.
//!
//! ## The pipeline is two fixed commands
//!
//! `sox` decodes whatever the sender pushed into the WAV that the transmitter expects, and
//! `pi_fm_rds -freq 108 -audio -` reads that WAV on stdin. Both are constants, not
//! user-supplied: this renderer exists to do exactly one thing on exactly one kind of
//! machine.
//!
//! ## Why `sudo` is checked only for the *target*
//!
//! `sudo` resolves the real binary itself, so a `PATH` lookup on `pi_fm_rds` catches the
//! common "not installed at all" case — it cannot catch "sudo will prompt". That limitation is
//! the reference's and is worth keeping visible rather than pretending the check is stronger.

/// The transmitter. `-freq 108 -audio -` means "108 MHz, read WAV from stdin".
pub const FM_COMMAND: &[&str] = &["sudo", "pi_fm_rds", "-freq", "108", "-audio", "-"];

/// The decoder feeding it.
pub const SOX_COMMAND: &[&str] = &["sox", "-t", "mp3"];

/// The program whose presence matters — **not** `FM_COMMAND[0]`, which is `sudo`.
pub const FM_PROGRAM: &str = "pi_fm_rds";

/// Why this renderer cannot run here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// The transmitter is not installed.
    NoTransmitter,
    /// The decoder is not installed.
    NoDecoder,
}

impl Unavailable {
    /// The sentence the user gets.
    ///
    /// The two are different on purpose: one names a Raspberry Pi program that has no package
    /// on most systems, the other names `sox`, which does. A single shared message would send
    /// half the readers to install the wrong thing.
    pub fn message(&self) -> String {
        match self {
            Unavailable::NoTransmitter => format!(
                "{FM_PROGRAM} is not installed; the PIFMRDS renderer only works on a \
                 Raspberry Pi with PiFmRds available"
            ),
            Unavailable::NoDecoder => format!(
                "{} is not installed; the PIFMRDS renderer needs it (see the plugin README)",
                SOX_COMMAND[0]
            ),
        }
    }
}

/// Check that both programs exist before anything is spawned.
///
/// `which` is the caller's `PATH` lookup. The order is the reference's: the transmitter first,
/// because it is the one that cannot be installed from an ordinary package manager, so it is
/// the more useful answer when both are missing.
pub fn preflight(which: impl Fn(&str) -> Option<String>) -> Result<(), Unavailable> {
    if which(FM_PROGRAM).is_none() {
        return Err(Unavailable::NoTransmitter);
    }
    if which(SOX_COMMAND[0]).is_none() {
        return Err(Unavailable::NoDecoder);
    }
    Ok(())
}

/// The argv pairs to hand the process spawner, in start order.
///
/// `pairs` rather than one entry per child because both are needed together: starting the
/// transmitter without a decoder gives a carrier with nothing on it, which sounds like the
/// renderer half-works.
pub fn pipeline() -> [&'static [&'static str]; 2] {
    [SOX_COMMAND, FM_COMMAND]
}

/// How the two children relate: `sox` writes into `pi_fm_rds`'s stdin.
///
/// Named rather than described, so the caller cannot accidentally give the transmitter a
/// terminal stdin (`-audio -` would then block forever, which is the hang in a new costume).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wiring {
    /// The decoder's stdout is the transmitter's stdin.
    DecoderIntoTransmitter,
}

pub const WIRING: Wiring = Wiring::DecoderIntoTransmitter;

/// What a start attempt may do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartPlan {
    /// Spawn both, wired together.
    Spawn,
    /// Already running; the reference returns early instead of starting a second transmitter.
    AlreadyRunning,
    /// Do not spawn; report this.
    Refuse(Unavailable),
}

/// Decide whether to start.
///
/// Running is an input, not something this module can see: the reference's check is
/// `self.fm is not None and self.fm.poll() is None`, and asking a child whether it is alive is
/// the caller's job. Two transmitters on the same frequency would interfere, which is why the
/// early return exists.
pub fn start_plan(running: bool, which: impl Fn(&str) -> Option<String>) -> StartPlan {
    if running {
        return StartPlan::AlreadyRunning;
    }
    match preflight(which) {
        Ok(()) => StartPlan::Spawn,
        Err(why) => StartPlan::Refuse(why),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has(programs: &'static [&'static str]) -> impl Fn(&str) -> Option<String> {
        move |name: &str| {
            programs
                .contains(&name)
                .then(|| format!("/usr/bin/{name}"))
        }
    }

    #[test]
    fn both_programs_present_means_spawn() {
        let plan = start_plan(false, has(&["pi_fm_rds", "sox"]));
        assert_eq!(plan, StartPlan::Spawn);
    }

    #[test]
    fn the_transmitter_is_checked_first_because_it_is_the_one_without_a_package() {
        // With both missing, the answer must name the Raspberry Pi program — installing `sox`
        // would not help, and a message that starts with `sox` sends the reader the wrong way.
        let plan = start_plan(false, has(&[]));
        match plan {
            StartPlan::Refuse(why) => assert_eq!(why, Unavailable::NoTransmitter),
            other => panic!("expected a refusal, got {other:?}"),
        }
        // …and when only the decoder is missing, it says so.
        let plan = start_plan(false, has(&["pi_fm_rds"]));
        match plan {
            StartPlan::Refuse(why) => assert_eq!(why, Unavailable::NoDecoder),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_program_refuses_instead_of_hanging() {
        // The upstream defect: spawning `sudo pi_fm_rds` unconditionally hung the app on a
        // machine without the binary. A refusal is the whole fix, and it must happen before
        // any spawn — which is why this function returns a plan rather than running anything.
        assert!(matches!(
            start_plan(false, has(&[])),
            StartPlan::Refuse(_)
        ));
        let why = preflight(has(&[])).expect_err("nothing is installed");
        assert!(why.message().contains("pi_fm_rds"));
        assert!(why.message().contains("Raspberry Pi"));
        assert!(Unavailable::NoDecoder.message().contains("sox"));
        assert!(Unavailable::NoDecoder.message().contains("README"));
    }

    #[test]
    fn an_already_running_transmitter_is_not_started_twice() {
        // Two transmitters on 108 MHz interfere with each other. The reference returns early
        // for the same reason; note that this answer wins even when everything is installed.
        assert_eq!(
            start_plan(true, has(&["pi_fm_rds", "sox"])),
            StartPlan::AlreadyRunning
        );
    }

    #[test]
    fn the_pipeline_is_two_fixed_commands_with_the_transmitter_reading_stdin() {
        // The `-audio -` is what makes the wiring legal: a transmitter given a terminal stdin
        // would block forever, which is the original hang in a new costume.
        let [decoder, transmitter] = pipeline();
        assert_eq!(decoder, SOX_COMMAND);
        assert_eq!(transmitter, FM_COMMAND);
        assert_eq!(FM_COMMAND[0], "sudo");
        assert_eq!(FM_PROGRAM, FM_COMMAND[1], "the check must skip `sudo`");
        assert_eq!(
            FM_COMMAND.last(),
            Some(&"-"),
            "the transmitter must read its audio from stdin"
        );
        assert_eq!(WIRING, Wiring::DecoderIntoTransmitter);
    }

    /// Every preflight verdict the reference gave, replayed against this port.
    ///
    /// Produced by executing the reference's `start_transmitter` guard followed by its
    /// `_require('sox')` over six synthetic `PATH` contents. The `sudo_only` and `sudo_sox`
    /// rows are the ones that matter: having `sudo` must prove nothing, because every machine
    /// has it.
    #[test]
    fn every_reference_preflight_replays_identically() {
        let corpus = include_str!("../tests/golden/pi_fm.jsonl");
        let mut rows = 0usize;
        for line in corpus.lines().filter(|line| !line.trim().is_empty()) {
            let row: serde_json::Value = serde_json::from_str(line).expect("corpus row");
            let has: Vec<String> = row["has"]
                .as_array()
                .expect("has")
                .iter()
                .map(|p| p.as_str().unwrap_or("").to_string())
                .collect();
            let which = |name: &str| {
                has.iter()
                    .any(|present| present == name)
                    .then(|| format!("/usr/bin/{name}"))
            };
            let verdict = match start_plan(false, which) {
                StartPlan::Spawn => "Spawn",
                StartPlan::Refuse(Unavailable::NoTransmitter) => "NoTransmitter",
                StartPlan::Refuse(Unavailable::NoDecoder) => "NoDecoder",
                StartPlan::AlreadyRunning => "AlreadyRunning",
            };
            assert_eq!(
                verdict,
                row["verdict"].as_str().unwrap_or(""),
                "row {} disagrees with the reference",
                row["case"]
            );
            rows += 1;
        }
        assert_eq!(rows, 6, "the corpus lost rows");
    }

    #[test]
    fn the_program_to_check_is_not_the_first_element() {
        // `shutil.which("sudo")` would succeed on every machine and prove nothing — the
        // reference's comment says `sudo` resolves the real binary itself.
        assert_ne!(FM_PROGRAM, FM_COMMAND[0]);
        let plan = start_plan(false, has(&["sudo", "sox"]));
        assert!(
            matches!(plan, StartPlan::Refuse(Unavailable::NoTransmitter)),
            "having `sudo` must not pass the check: {plan:?}"
        );
    }
}
