//! Targets whose render sender creation went unanswered, remembered across runs.
//!
//! A target listed as running may not answer a sender creation, which then runs into its 10 s
//! timeout. So that such a target does not delay every start, a
//! peer whose creation timed out is held back for [`UNANSWERED_MEMORY`], also in the runs that
//! follow, until it answers, asks for a stream, its listed state changes or it is listed under
//! another version id. A held-back peer is still retried in the background, once per retry
//! period after its last timeout ([`UnansweredPeers::is_retry_due`]), so a headset switched on
//! later under the same listing is found.
//!
//! The file holds one line per peer: the timeout in Unix seconds, the peer id and the version
//! id it was listed with, separated by single spaces.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use qaros::PeerId;

/// How long a peer whose creation timed out is held back.
pub const UNANSWERED_MEMORY: Duration = Duration::from_mins(10);

#[derive(Clone, Debug, PartialEq, Eq)]
struct Unanswered {
    version_id: String,
    timed_out_at: SystemTime,
}

impl Unanswered {
    /// A timeout stamped in the future (a clock set back) holds nothing back.
    fn is_recent(&self, now: SystemTime) -> bool {
        now.duration_since(self.timed_out_at)
            .is_ok_and(|age| age < UNANSWERED_MEMORY)
    }
}

#[derive(Debug, Default)]
pub struct UnansweredPeers {
    /// Rewritten on every change; `None` remembers for this run only.
    file: Option<PathBuf>,
    peers: BTreeMap<PeerId, Unanswered>,
}

impl UnansweredPeers {
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// The peers `file` remembers from the last [`UNANSWERED_MEMORY`]. A missing file
    /// remembers none; an unreadable file or malformed lines are logged and skipped.
    pub fn load(file: PathBuf, now: SystemTime) -> Self {
        let peers = match std::fs::read_to_string(&file) {
            Ok(text) => parse_unanswered(&text, &file),
            Err(error) if error.kind() == ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => {
                log::warn!(
                    "qaros: cannot read the unanswered peers from {}: {error}; starting without \
                     them",
                    file.display()
                );
                BTreeMap::new()
            }
        };
        let mut remembered = Self {
            file: Some(file),
            peers,
        };
        remembered
            .peers
            .retain(|_, unanswered| unanswered.is_recent(now));
        if !remembered.peers.is_empty() {
            log::info!(
                "qaros: held back, retried in the background, having not answered in the last \
                 {UNANSWERED_MEMORY:?}: {}",
                remembered.describe_peers()
            );
        }
        remembered
    }

    fn describe_peers(&self) -> String {
        self.peers
            .iter()
            .map(|(peer, unanswered)| format!("{peer} (version {:?})", unanswered.version_id))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// True while the peer, listed under `version_id`, is held back. A peer listed under
    /// another version id restarted, and one whose time is up may be tried: both are
    /// forgotten.
    pub fn holds_back(&mut self, peer: PeerId, version_id: &str, now: SystemTime) -> bool {
        let Some(unanswered) = self.peers.get(&peer) else {
            return false;
        };
        if unanswered.version_id != version_id {
            self.forget(peer, "it is listed under another version id");
            return false;
        }
        if !unanswered.is_recent(now) {
            self.forget(peer, "its time is up");
            return false;
        }
        true
    }

    /// A held-back peer's last timeout lies `retry_period` back or more; true for a peer not
    /// held back.
    pub fn is_retry_due(&self, peer: PeerId, now: SystemTime, retry_period: Duration) -> bool {
        self.peers.get(&peer).is_none_or(|unanswered| {
            now.duration_since(unanswered.timed_out_at)
                .is_ok_and(|age| age >= retry_period)
        })
    }

    pub fn remember(&mut self, peer: PeerId, version_id: &str, now: SystemTime) {
        self.peers.insert(
            peer,
            Unanswered {
                version_id: version_id.to_owned(),
                timed_out_at: now,
            },
        );
        log::info!(
            "qaros: target {peer} held back for {UNANSWERED_MEMORY:?}, also in later runs, \
             unless it answers a background retry, asks for a stream or restarts"
        );
        self.save(now);
    }

    /// The peer is no longer held back; `news` says why, for the log.
    pub fn forget(&mut self, peer: PeerId, news: &str) {
        if self.peers.remove(&peer).is_some() {
            log::info!("qaros: target {peer} no longer held back: {news}");
            self.save(SystemTime::now());
        }
    }

    fn save(&mut self, now: SystemTime) {
        self.peers.retain(|_, unanswered| unanswered.is_recent(now));
        let Some(file) = &self.file else {
            return;
        };
        let mut text = String::new();
        for (peer, unanswered) in &self.peers {
            let seconds = unanswered
                .timed_out_at
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since_epoch| since_epoch.as_secs());
            let _ = writeln!(text, "{seconds} {peer} {}", unanswered.version_id);
        }
        let temporary = file.with_file_name(temporary_file_name(file));
        let written =
            std::fs::write(&temporary, text).and_then(|()| std::fs::rename(&temporary, file));
        if let Err(error) = written {
            if let Err(cleanup) = std::fs::remove_file(&temporary)
                && cleanup.kind() != ErrorKind::NotFound
            {
                log::warn!("qaros: cannot remove {}: {cleanup}", temporary.display());
            }
            log::warn!(
                "qaros: cannot store the unanswered peers in {}: {error}; they are held back \
                 in this run only",
                file.display()
            );
        }
    }
}

/// Unique per process and write, so viewers sharing a directory do not write into each
/// other's temporary file.
fn temporary_file_name(file: &std::path::Path) -> std::ffi::OsString {
    static WRITES: AtomicU64 = AtomicU64::new(0);
    let mut name = file.file_name().unwrap_or_default().to_owned();
    name.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        WRITES.fetch_add(1, Ordering::Relaxed)
    ));
    name
}

fn parse_unanswered(text: &str, file: &std::path::Path) -> BTreeMap<PeerId, Unanswered> {
    let mut peers = BTreeMap::new();
    let mut malformed_lines = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match parse_line(line) {
            Some((peer, unanswered)) => {
                peers.insert(peer, unanswered);
            }
            None => malformed_lines.push(index + 1),
        }
    }
    if !malformed_lines.is_empty() {
        log::warn!(
            "qaros: skipped malformed lines {malformed_lines:?} of the unanswered peers in {}",
            file.display()
        );
    }
    peers
}

fn parse_line(line: &str) -> Option<(PeerId, Unanswered)> {
    let mut fields = line.splitn(3, ' ');
    let seconds: u64 = fields.next()?.parse().ok()?;
    let peer = parse_peer_id(fields.next()?)?;
    let version_id = fields.next().unwrap_or_default().to_owned();
    Some((
        peer,
        Unanswered {
            version_id,
            timed_out_at: UNIX_EPOCH + Duration::from_secs(seconds),
        },
    ))
}

/// The colon-separated hex bytes `PeerId` formats as.
fn parse_peer_id(text: &str) -> Option<PeerId> {
    let mut bytes = [0_u8; 16];
    let mut hex_bytes = text.split(':');
    for byte in &mut bytes {
        let hex_byte = hex_bytes.next()?;
        if hex_byte.len() != 2 {
            return None;
        }
        *byte = u8::from_str_radix(hex_byte, 16).ok()?;
    }
    hex_bytes.next().is_none().then_some(PeerId(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEAD_RECEIVER: PeerId = PeerId([0xde; 16]);
    const OTHER_RECEIVER: PeerId = PeerId([0x0a; 16]);

    fn scratch_file(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "qaros-sink-unanswered-{name}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        directory.join("qaros-rerun.unanswered-peers.txt")
    }

    #[test]
    fn a_remembered_peer_is_held_back_in_the_next_run_until_its_time_is_up() {
        let file = scratch_file("next-run");
        let timed_out_at = UNIX_EPOCH + Duration::from_secs(1_800_000_123);
        let mut first_run = UnansweredPeers::load(file.clone(), timed_out_at);
        first_run.remember(DEAD_RECEIVER, "0.9.0", timed_out_at);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            format!("1800000123 {DEAD_RECEIVER} 0.9.0\n")
        );

        let next_start = timed_out_at + Duration::from_mins(3);
        let mut next_run = UnansweredPeers::load(file.clone(), next_start);
        assert!(next_run.holds_back(DEAD_RECEIVER, "0.9.0", next_start));
        assert!(!next_run.holds_back(OTHER_RECEIVER, "0.9.0", next_start));
        let time_up = timed_out_at + UNANSWERED_MEMORY;
        assert!(!next_run.holds_back(DEAD_RECEIVER, "0.9.0", time_up));
        assert!(!next_run.holds_back(DEAD_RECEIVER, "0.9.0", next_start));

        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    #[test]
    fn another_version_id_or_a_forget_lifts_the_hold_and_the_file_follows() {
        let file = scratch_file("lifted");
        let now = SystemTime::now();
        let mut remembered = UnansweredPeers::load(file.clone(), now);
        remembered.remember(DEAD_RECEIVER, "build-a", now);
        remembered.remember(OTHER_RECEIVER, "build-a", now);

        assert!(!remembered.holds_back(DEAD_RECEIVER, "build-b", now));
        assert!(
            !remembered.holds_back(DEAD_RECEIVER, "build-a", now),
            "forgotten"
        );
        remembered.forget(OTHER_RECEIVER, "it asked for a stream");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "");

        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    #[test]
    fn malformed_lines_are_skipped_and_the_rest_is_read() {
        let file = scratch_file("malformed");
        let now = UNIX_EPOCH + Duration::from_secs(1_800_000_123);
        std::fs::write(
            &file,
            format!(
                "1800000123 {DEAD_RECEIVER} build with spaces\nnot a line\n1800000123 DE:AD\n\n\
                 1800000123 {OTHER_RECEIVER}\n"
            ),
        )
        .unwrap();

        let mut remembered = UnansweredPeers::load(file.clone(), now);
        assert!(remembered.holds_back(DEAD_RECEIVER, "build with spaces", now));
        assert!(remembered.holds_back(OTHER_RECEIVER, "", now));
        assert_eq!(remembered.peers.len(), 2);

        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    #[test]
    fn a_held_back_peer_is_due_for_a_retry_a_retry_period_after_its_timeout() {
        let timed_out_at = UNIX_EPOCH + Duration::from_secs(1_800_000_123);
        let mut remembered = UnansweredPeers::in_memory();
        remembered.remember(DEAD_RECEIVER, "build-a", timed_out_at);
        let period = Duration::from_mins(1);
        assert!(!remembered.is_retry_due(
            DEAD_RECEIVER,
            timed_out_at + Duration::from_secs(59),
            period
        ));
        assert!(remembered.is_retry_due(DEAD_RECEIVER, timed_out_at + period, period));
        assert!(remembered.is_retry_due(OTHER_RECEIVER, timed_out_at, period));
    }

    #[test]
    fn temporary_file_names_differ_per_write_and_carry_the_process_id() {
        let file = std::path::Path::new("directory/peers.txt");
        let first = temporary_file_name(file).into_string().unwrap();
        let second = temporary_file_name(file).into_string().unwrap();
        assert_ne!(first, second);
        let write_counter = first
            .strip_prefix(&format!("peers.txt.{}.", std::process::id()))
            .and_then(|rest| rest.strip_suffix(".tmp"))
            .unwrap_or_else(|| panic!("{first}"));
        assert!(write_counter.parse::<u64>().is_ok(), "{first}");
    }

    #[test]
    fn a_peer_id_reads_back_from_its_display_form_only() {
        assert_eq!(
            parse_peer_id(&DEAD_RECEIVER.to_string()),
            Some(DEAD_RECEIVER)
        );
        assert_eq!(parse_peer_id("DE:AD"), None);
        assert_eq!(parse_peer_id(&format!("{DEAD_RECEIVER}:00")), None);
        assert_eq!(
            parse_peer_id(&DEAD_RECEIVER.to_string().replace(':', "")),
            None
        );
    }

    #[test]
    fn a_file_that_cannot_be_written_keeps_the_peers_for_this_run() {
        let directory = scratch_file("unwritable");
        let file = directory.join("missing-directory").join("peers.txt");
        let now = SystemTime::now();
        let mut remembered = UnansweredPeers::load(file.clone(), now);
        remembered.remember(DEAD_RECEIVER, "build-a", now);
        assert!(remembered.holds_back(DEAD_RECEIVER, "build-a", now));
        assert!(!file.exists());

        let _ = std::fs::remove_dir_all(directory.parent().unwrap());
    }
}
