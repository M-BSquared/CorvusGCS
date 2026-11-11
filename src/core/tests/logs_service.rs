//! The log service against a vehicle that answers the log protocol from memory
//! (`python/tests/test_log_service.py`, case for case).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use corvus_core::logs::localtime;
use corvus_core::logs::service::{
    safe_component, tlog_started, LogLink, LogMsg, LogService, LogSink, Timing, BUSY_STATES,
};

#[derive(Default)]
struct State {
    logs: BTreeMap<i64, Vec<u8>>,
    connected: bool,
    erase_calls: u32,
    erase_ok: bool,
    command_error: String,
    list_requests: u32,
    data_requests: Vec<(i64, i64, i64)>,
    end_requests: u32,
    drop_first_chunk: bool,
    dropped: BTreeSet<(i64, i64)>,
    drop_list_requests: u32,
    chunk_delay: Duration,
    require_session: bool,
    ignored_data_requests: u32,
    session: bool,
    claimed_size: BTreeMap<i64, i64>,
    suffix: &'static str,
}

struct Fake {
    st: Mutex<State>,
    sink: Mutex<Option<LogSink>>,
    detached: AtomicBool,
}

impl Fake {
    fn new(logs: &[(i64, Vec<u8>)]) -> Arc<Fake> {
        Arc::new(Fake {
            st: Mutex::new(State {
                logs: logs.iter().cloned().collect(),
                connected: true,
                erase_ok: true,
                suffix: ".ulg",
                ..State::default()
            }),
            sink: Mutex::new(None),
            detached: AtomicBool::new(true),
        })
    }

    fn with<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        f(&mut self.st.lock().unwrap())
    }

    fn sink(&self) -> Option<LogSink> {
        self.sink.lock().unwrap().clone()
    }
}

impl LogLink for Fake {
    fn is_connected(&self) -> bool {
        self.with(|s| s.connected)
    }

    fn set_log_sink(&self, sink: Option<LogSink>) {
        self.detached.store(sink.is_none(), Ordering::SeqCst);
        *self.sink.lock().unwrap() = sink;
    }

    fn request_log_list(&self, _start: i64, _end: i64) -> bool {
        let entries: Vec<(i64, i64)> = {
            let mut s = self.st.lock().unwrap();
            s.list_requests += 1;
            if s.drop_list_requests > 0 {
                s.drop_list_requests -= 1;
                return true;
            }
            s.session = true;
            s.logs
                .iter()
                .map(|(id, blob)| (*id, *s.claimed_size.get(id).unwrap_or(&(blob.len() as i64))))
                .collect()
        };
        let total = entries.len() as i64;
        for (id, size) in entries {
            let Some(sink) = self.sink() else { break };
            sink(&LogMsg::Entry {
                id,
                size,
                utc: 1_700_000_000 + id,
                num_logs: total,
            });
        }
        true
    }

    fn request_log_data(&self, id: i64, offset: i64, count: i64) -> bool {
        let (blob, drop_first, delay) = {
            let mut s = self.st.lock().unwrap();
            s.data_requests.push((id, offset, count));
            if s.require_session && !s.session {
                s.ignored_data_requests += 1;
                return true;
            }
            (
                s.logs.get(&id).cloned().unwrap_or_default(),
                s.drop_first_chunk,
                s.chunk_delay,
            )
        };
        let mut sent = 0i64;
        while sent < count {
            let Some(sink) = self.sink() else { break };
            let at = offset + sent;
            if at as usize >= blob.len() {
                break;
            }
            let end = (at as usize + 90).min(blob.len());
            let chunk = &blob[at as usize..end];
            let skip = drop_first_chunk(self, drop_first, id, at);
            sent += chunk.len() as i64;
            if skip {
                continue;
            }
            let mut data = chunk.to_vec();
            data.resize(90, 0);
            sink(&LogMsg::Data {
                id,
                ofs: at,
                count: chunk.len() as i64,
                data,
            });
            if !delay.is_zero() {
                std::thread::sleep(delay);
            }
        }
        true
    }

    fn log_request_end(&self) -> bool {
        self.with(|s| {
            s.end_requests += 1;
            s.session = false;
        });
        true
    }

    fn erase_logs(&self) -> bool {
        self.with(|s| {
            s.erase_calls += 1;
            if !s.erase_ok {
                return false;
            }
            s.logs.clear();
            true
        })
    }

    fn last_command_error(&self) -> String {
        self.with(|s| s.command_error.clone())
    }

    fn log_suffix(&self) -> &'static str {
        self.with(|s| s.suffix)
    }
}

fn drop_first_chunk(fake: &Fake, enabled: bool, id: i64, at: i64) -> bool {
    enabled && fake.with(|s| s.dropped.insert((id, at)))
}

fn fast() -> Timing {
    Timing {
        list_timeout: 0.5,
        list_retry: 0.05,
        data_stall_timeout: 1.0,
        data_quiet: 0.1,
        download_give_up: 5.0,
        prime_timeout: 0.3,
        erase_settle: 0.01,
    }
}

fn dir_of(path: &Path) -> Arc<dyn Fn() -> String + Send + Sync> {
    let text = path.to_string_lossy().into_owned();
    Arc::new(move || text.clone())
}

fn service(dir: &Path, fake: &Arc<Fake>) -> Arc<LogService> {
    LogService::with_timing(fake.clone(), dir_of(dir), Some(dir_of(dir)), fast())
}

fn wait_idle(service: &LogService, secs: f64) -> serde_json::Value {
    let limit = Instant::now() + Duration::from_secs_f64(secs);
    while Instant::now() < limit {
        let status = service.status();
        if !BUSY_STATES.contains(&status["state"].as_str().unwrap()) {
            return status;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("log service stuck in {}", service.status()["state"]);
}

fn files(dir: &Path, suffix: &str) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(suffix))
        .collect();
    out.sort();
    out
}

fn tmp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("corvus-logs-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn blob(byte: u8, n: usize) -> Vec<u8> {
    vec![byte; n]
}

// The vehicle's log session

#[test]
fn a_download_reopens_the_session_a_stale_listing_left_closed() {
    let dir = tmp("reopen");
    let fake = Fake::new(&[(1, blob(b'a', 900))]);
    fake.with(|s| s.require_session = true);
    let svc = service(&dir, &fake);
    assert!(svc.refresh());
    wait_idle(&svc, 10.0);
    fake.log_request_end();

    assert!(svc.start_download(&[1]));
    let status = wait_idle(&svc, 10.0);
    assert_eq!(status["state"], "done", "{}", status["message"]);
    assert_eq!(fake.with(|s| s.ignored_data_requests), 0);
    let saved = files(&dir, ".ulg");
    assert_eq!(saved.len(), 1);
    assert_eq!(std::fs::read(dir.join(&saved[0])).unwrap(), blob(b'a', 900));
    svc.shutdown();
}

#[test]
fn a_second_batch_downloads_after_the_first_ended_the_session() {
    let dir = tmp("batches");
    let fake = Fake::new(&[(1, blob(b'a', 400)), (2, blob(b'b', 400))]);
    fake.with(|s| s.require_session = true);
    let svc = service(&dir, &fake);
    svc.refresh();
    wait_idle(&svc, 10.0);

    assert!(svc.start_download(&[1]));
    assert_eq!(wait_idle(&svc, 10.0)["state"], "done");
    assert!(fake.with(|s| s.end_requests) >= 1);
    assert!(svc.start_download(&[2]));
    assert_eq!(wait_idle(&svc, 10.0)["state"], "done");
    assert_eq!(fake.with(|s| s.ignored_data_requests), 0);
    assert_eq!(files(&dir, ".ulg").len(), 2);
    svc.shutdown();
}

// Listing

#[test]
fn a_listing_collects_entries_and_keeps_the_session_open() {
    let dir = tmp("listing");
    let fake = Fake::new(&[(1, blob(b'a', 200)), (2, blob(b'b', 100))]);
    let svc = service(&dir, &fake);
    assert!(svc.refresh());
    let status = wait_idle(&svc, 10.0);
    let ids: Vec<i64> = status["logs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, [2, 1], "newest first");
    assert_eq!(status["logs"][1]["size"], 200);
    assert_eq!(fake.with(|s| s.end_requests), 0);
    assert!(
        fake.detached.load(Ordering::SeqCst),
        "the sink is detached when the job ends"
    );
    svc.shutdown();
}

#[test]
fn a_lost_list_request_is_retried_not_reported_as_an_empty_card() {
    let dir = tmp("retry");
    let fake = Fake::new(&[(1, blob(b'a', 200))]);
    fake.with(|s| s.drop_list_requests = 2);
    let svc = service(&dir, &fake);
    svc.refresh();
    let status = wait_idle(&svc, 10.0);
    assert_eq!(fake.with(|s| s.list_requests), 3);
    assert_eq!(status["logs"][0]["id"], 1);
    svc.shutdown();
}

#[test]
fn a_silent_vehicle_is_asked_repeatedly_before_being_called_empty() {
    let dir = tmp("silent");
    let fake = Fake::new(&[(1, blob(b'a', 200))]);
    fake.with(|s| s.drop_list_requests = 99);
    let svc = service(&dir, &fake);
    svc.refresh();
    let status = wait_idle(&svc, 10.0);
    assert_eq!(fake.with(|s| s.list_requests), 4);
    assert_eq!(status["state"], "idle");
    assert_eq!(status["message"], "No logs on the vehicle");
    assert_eq!(status["logs"], serde_json::json!([]));
    svc.shutdown();
}

#[test]
fn empty_log_slots_are_not_offered() {
    let dir = tmp("empty-slots");
    let fake = Fake::new(&[(1, Vec::new()), (2, blob(b'x', 90))]);
    let svc = service(&dir, &fake);
    svc.refresh();
    let status = wait_idle(&svc, 10.0);
    assert_eq!(status["logs"].as_array().unwrap().len(), 1);
    assert_eq!(status["logs"][0]["id"], 2);
    svc.shutdown();
}

#[test]
fn listing_refuses_without_a_link() {
    let dir = tmp("nolink");
    let fake = Fake::new(&[]);
    fake.with(|s| s.connected = false);
    let svc = service(&dir, &fake);
    assert!(!svc.refresh());
    assert!(svc.last_error().contains("not connected"));
    assert_eq!(svc.status()["connected"], false);
    svc.shutdown();
}

// Download

#[test]
fn a_download_writes_the_exact_bytes() {
    let dir = tmp("exact");
    let data: Vec<u8> = (0..=255u8).cycle().take(1024).collect();
    let fake = Fake::new(&[(7, data.clone())]);
    let svc = service(&dir, &fake);
    svc.refresh();
    wait_idle(&svc, 10.0);
    assert!(svc.start_download(&[7]));
    let status = wait_idle(&svc, 10.0);
    assert_eq!(status["state"], "done", "{}", status["message"]);
    let written: Vec<String> = files(&dir, ".ulg")
        .into_iter()
        .filter(|n| n.starts_with("log_007"))
        .collect();
    assert_eq!(written.len(), 1);
    assert_eq!(std::fs::read(dir.join(&written[0])).unwrap(), data);
    assert!(files(&dir, ".part").is_empty());
    svc.shutdown();
}

#[test]
fn a_lost_packet_is_re_requested_not_written_as_a_hole() {
    let dir = tmp("hole");
    let data: Vec<u8> = (0..=255u8).cycle().take(2048).collect();
    let fake = Fake::new(&[(3, data.clone())]);
    fake.with(|s| s.drop_first_chunk = true);
    let svc = service(&dir, &fake);
    svc.refresh();
    wait_idle(&svc, 10.0);
    svc.start_download(&[3]);
    let status = wait_idle(&svc, 20.0);
    assert_eq!(status["state"], "done", "{}", status["message"]);
    let name = files(&dir, ".ulg")
        .into_iter()
        .find(|n| n.starts_with("log_003"))
        .unwrap();
    assert_eq!(std::fs::read(dir.join(name)).unwrap(), data);
    svc.shutdown();
}

#[test]
fn several_logs_download_one_after_another() {
    let dir = tmp("queue");
    let fake = Fake::new(&[(1, blob(b'1', 300)), (2, blob(b'2', 300)), (3, blob(b'3', 500))]);
    let svc = service(&dir, &fake);
    svc.refresh();
    wait_idle(&svc, 10.0);
    assert!(svc.start_download(&[1, 2, 3]));
    let status = wait_idle(&svc, 20.0);
    assert_eq!(status["state"], "done", "{}", status["message"]);
    assert_eq!(files(&dir, ".ulg").len(), 3);
    let ok: Vec<bool> = status["completed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["ok"].as_bool().unwrap())
        .collect();
    assert_eq!(ok, [true, true, true]);
    assert!(!fake.with(|s| s.data_requests.is_empty()));
    svc.shutdown();
}

#[test]
fn a_second_job_is_refused_while_one_runs() {
    let dir = tmp("busy");
    let fake = Fake::new(&[(1, blob(b'x', 90_000))]);
    fake.with(|s| s.chunk_delay = Duration::from_millis(1));
    let svc = service(&dir, &fake);
    svc.refresh();
    wait_idle(&svc, 10.0);
    svc.start_download(&[1]);
    assert!(!svc.start_download(&[1]));
    assert!(svc.last_error().contains("already running"));
    svc.cancel();
    wait_idle(&svc, 20.0);
    svc.shutdown();
}

#[test]
fn a_download_refuses_an_unknown_log() {
    let dir = tmp("unknown");
    let fake = Fake::new(&[(1, blob(b'x', 90))]);
    let svc = service(&dir, &fake);
    svc.refresh();
    wait_idle(&svc, 10.0);
    assert!(!svc.start_download(&[99]));
    assert!(svc.last_error().contains("no known log"));
    svc.shutdown();
}

#[test]
fn cancel_stops_the_queue_and_leaves_no_partial_file() {
    let dir = tmp("cancel");
    let fake = Fake::new(&[(1, blob(b'x', 400_000)), (2, blob(b'y', 400_000))]);
    fake.with(|s| s.chunk_delay = Duration::from_millis(1));
    let svc = service(&dir, &fake);
    svc.refresh();
    wait_idle(&svc, 10.0);
    svc.start_download(&[1, 2]);
    std::thread::sleep(Duration::from_millis(50));
    assert!(svc.cancel());
    let status = wait_idle(&svc, 20.0);
    assert_eq!(status["state"], "cancelled");
    assert!(files(&dir, ".part").is_empty());
    svc.shutdown();
}

#[test]
fn one_failed_log_does_not_abandon_the_rest_of_the_queue() {
    let dir = tmp("failed-one");
    let fake = Fake::new(&[(1, blob(b'a', 90)), (2, blob(b'b', 90))]);
    fake.with(|s| {
        s.claimed_size.insert(1, 1_000_000_000_000);
    });
    let svc = service(&dir, &fake);
    svc.refresh();
    wait_idle(&svc, 10.0);
    svc.start_download(&[1, 2]);
    let status = wait_idle(&svc, 20.0);
    let results: Vec<(i64, bool)> = status["completed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["id"].as_i64().unwrap(), c["ok"].as_bool().unwrap()))
        .collect();
    assert_eq!(results, [(1, false), (2, true)]);
    assert!(files(&dir, ".ulg").iter().any(|n| n.starts_with("log_002")));
    assert_eq!(status["state"], "failed");
    assert_eq!(status["message"], "1 saved, 1 failed");
    svc.shutdown();
}

// Erase

#[test]
fn an_erase_clears_the_vehicle_and_re_reads_the_result() {
    let dir = tmp("erase");
    let fake = Fake::new(&[(1, blob(b'a', 90)), (2, blob(b'b', 90))]);
    let svc = service(&dir, &fake);
    svc.refresh();
    assert_eq!(wait_idle(&svc, 10.0)["logs"].as_array().unwrap().len(), 2);
    assert!(svc.erase());
    let status = wait_idle(&svc, 20.0);
    assert_eq!(fake.with(|s| s.erase_calls), 1);
    assert_eq!(status["logs"], serde_json::json!([]));
    assert_eq!(status["message"], "No logs on the vehicle");
    svc.shutdown();
}

#[test]
fn a_refused_erase_is_reported_with_the_vehicles_reason() {
    let dir = tmp("erase-refused");
    let fake = Fake::new(&[(1, blob(b'a', 90))]);
    fake.with(|s| {
        s.erase_ok = false;
        s.command_error = "cannot erase logs while armed".into();
    });
    let svc = service(&dir, &fake);
    svc.refresh();
    wait_idle(&svc, 10.0);
    svc.erase();
    let status = wait_idle(&svc, 10.0);
    assert_eq!(status["state"], "failed");
    assert!(status["message"].as_str().unwrap().contains("armed"));
    assert_eq!(status["logs"].as_array().unwrap().len(), 1);
    svc.shutdown();
}

#[test]
fn an_erase_refuses_without_a_link() {
    let dir = tmp("erase-nolink");
    let fake = Fake::new(&[]);
    fake.with(|s| s.connected = false);
    let svc = service(&dir, &fake);
    assert!(!svc.erase());
    assert!(svc.last_error().contains("not connected"));
    svc.shutdown();
}

#[test]
fn an_erase_refuses_while_another_log_job_runs() {
    let dir = tmp("erase-busy");
    let fake = Fake::new(&[(1, blob(b'x', 400_000))]);
    fake.with(|s| s.chunk_delay = Duration::from_millis(1));
    let svc = service(&dir, &fake);
    svc.refresh();
    wait_idle(&svc, 10.0);
    svc.start_download(&[1]);
    assert!(!svc.erase());
    assert!(svc.last_error().contains("already running"));
    assert_eq!(fake.with(|s| s.erase_calls), 0);
    svc.cancel();
    wait_idle(&svc, 20.0);
    svc.shutdown();
}

#[test]
fn an_erase_does_not_touch_files_already_downloaded() {
    let dir = tmp("erase-keeps");
    let fake = Fake::new(&[(1, blob(b'a', 90))]);
    let svc = service(&dir, &fake);
    svc.refresh();
    wait_idle(&svc, 10.0);
    svc.start_download(&[1]);
    wait_idle(&svc, 10.0);
    let saved = files(&dir, ".ulg");
    assert!(!saved.is_empty());
    svc.erase();
    wait_idle(&svc, 20.0);
    assert_eq!(files(&dir, ".ulg"), saved);
    svc.shutdown();
}

// Files and folders

#[test]
fn a_log_already_in_the_folder_is_marked_downloaded() {
    let dir = tmp("marked");
    let fake = Fake::new(&[(1, blob(b'a', 90)), (2, blob(b'b', 180))]);
    let svc = service(&dir, &fake);
    svc.refresh();
    wait_idle(&svc, 10.0);
    svc.start_download(&[1]);
    wait_idle(&svc, 10.0);
    let status = svc.status();
    let by_id = |id: i64| {
        status["logs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["id"] == id)
            .unwrap()
            .clone()
    };
    let one = by_id(1);
    assert_eq!(one["downloaded"], true);
    assert!(one["file"].as_str().unwrap().ends_with(".ulg"));
    assert_eq!(
        one["file_name"].as_str().unwrap(),
        Path::new(one["file"].as_str().unwrap())
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
    );
    let two = by_id(2);
    assert_eq!(
        (
            two["downloaded"].as_bool(),
            two["file"].as_str(),
            two["file_name"].as_str()
        ),
        (Some(false), Some(""), Some(""))
    );
    svc.shutdown();
}

#[test]
fn a_same_numbered_file_of_a_different_size_is_not_claimed() {
    let dir = tmp("recycled");
    let fake = Fake::new(&[(3, blob(b'x', 900))]);
    let svc = service(&dir, &fake);
    svc.refresh();
    wait_idle(&svc, 10.0);
    std::fs::write(dir.join("log_003_2020-01-01_00-00.ulg"), blob(b'y', 42)).unwrap();
    assert_eq!(svc.status()["logs"][0]["downloaded"], false);
    svc.shutdown();
}

#[test]
fn saved_logs_lists_the_folder_and_recovers_ids() {
    let dir = tmp("saved");
    std::fs::write(dir.join("log_007_2026-01-01_10-00.ulg"), blob(b'x', 10)).unwrap();
    std::fs::write(dir.join("handcopied.ulg"), blob(b'y', 20)).unwrap();
    std::fs::write(dir.join("flight.bin"), blob(b'z', 5)).unwrap();
    std::fs::write(dir.join("notes.txt"), b"ignore").unwrap();
    let svc = service(&dir, &Fake::new(&[]));
    let saved = svc.saved_logs();
    let id_of = |name: &str| saved.iter().find(|f| f["name"] == name).map(|f| f["id"].clone());
    assert_eq!(saved.len(), 3);
    assert_eq!(id_of("log_007_2026-01-01_10-00.ulg"), Some(serde_json::json!(7)));
    assert_eq!(id_of("handcopied.ulg"), Some(serde_json::Value::Null));
    assert!(id_of("notes.txt").is_none());
    svc.shutdown();
}

#[test]
fn filenames_are_sortable_and_cannot_escape_the_folder() {
    let svc = service(Path::new("/tmp"), &Fake::new(&[]));
    let name = svc.filename(7, 1_700_000_000);
    assert!(name.starts_with("log_007_") && name.ends_with(".ulg"));
    assert!(!name.contains('/') && !name.contains(".."));
    assert_eq!(safe_component("../../etc/passwd"), "etc-passwd");
    assert_eq!(safe_component(""), "log");
}

#[test]
fn the_filename_stamp_is_this_machines_clock_not_utc() {
    let svc = service(Path::new("/tmp"), &Fake::new(&[]));
    let stamp = localtime::localtime(1_700_000_000).unwrap().stamp();
    assert_eq!(svc.filename(7, 1_700_000_000), format!("log_007_{stamp}.ulg"));
}

#[test]
fn a_log_the_vehicle_never_stamped_still_gets_a_name() {
    assert_eq!(
        service(Path::new("/tmp"), &Fake::new(&[])).filename(3, 0),
        "log_003.ulg"
    );
}

#[test]
fn an_ardupilot_log_is_named_bin() {
    let fake = Fake::new(&[]);
    fake.with(|s| s.suffix = ".bin");
    assert_eq!(service(Path::new("/tmp"), &fake).filename(3, 0), "log_003.bin");
}

#[test]
fn a_missing_download_folder_is_created() {
    let base = tmp("create");
    let target = base.join("nested").join("flightlogs");
    let fake = Fake::new(&[(1, blob(b'x', 90))]);
    let svc = LogService::with_timing(fake, dir_of(&target), None, fast());
    svc.refresh();
    wait_idle(&svc, 10.0);
    assert!(svc.start_download(&[1]));
    wait_idle(&svc, 10.0);
    assert!(target.is_dir());
    svc.shutdown();
}

#[test]
fn no_download_folder_refuses_the_download() {
    let fake = Fake::new(&[(1, blob(b'x', 90))]);
    let svc = LogService::with_timing(fake, Arc::new(String::new), None, fast());
    svc.refresh();
    wait_idle(&svc, 10.0);
    assert!(!svc.start_download(&[1]));
    assert_eq!(svc.last_error(), "no download folder configured");
    svc.shutdown();
}

#[test]
fn local_tlogs_are_listed() {
    let dir = tmp("tlogs");
    std::fs::write(dir.join("corvus_2026-01-01.tlog"), blob(b'x', 32)).unwrap();
    std::fs::write(dir.join("notes.txt"), b"ignore me").unwrap();
    let names: Vec<String> = service(&dir, &Fake::new(&[]))
        .local_tlogs()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["corvus_2026-01-01.tlog"]);
}

#[test]
fn tlogs_carry_when_the_recording_opened() {
    let dir = tmp("tlog-start");
    std::fs::write(dir.join("20260910-143205-123456.tlog"), blob(b'x', 8)).unwrap();
    let entry = service(&dir, &Fake::new(&[])).local_tlogs().remove(0);
    let started = entry["started"].as_f64().unwrap();
    assert_eq!(
        localtime::localtime(started.floor() as i64).unwrap().full(),
        "2026-09-10 14:32:05"
    );
    assert!((started.fract() - 0.123456).abs() < 1e-3);
}

#[test]
fn a_handmade_tlog_falls_back_to_its_mtime() {
    assert_eq!(tlog_started("from-a-colleague.tlog", 1_700_000_000.0), 1_700_000_000.0);
    assert_eq!(
        tlog_started("20261399-143205.tlog", 5.0),
        tlog_started("20261399-143205.tlog", 5.0)
    );
    assert_eq!(tlog_started("2026091-143205.tlog", 5.0), 5.0);
}

#[test]
fn a_de_collision_suffix_adds_its_microseconds() {
    let plain = tlog_started("20260910-143205.tlog", 0.0);
    let suffixed = tlog_started("20260910-143205-5.tlog", 0.0);
    assert!((suffixed - plain - 0.5).abs() < 1e-9);
}

#[test]
fn tlogs_come_back_newest_session_first() {
    let dir = tmp("tlog-order");
    for name in [
        "20260910-090000-000000.tlog",
        "20260910-143205-000000.tlog",
        "20260909-235900-000000.tlog",
    ] {
        std::fs::write(dir.join(name), blob(b'x', 8)).unwrap();
    }
    let names: Vec<String> = service(&dir, &Fake::new(&[]))
        .local_tlogs()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        names,
        [
            "20260910-143205-000000.tlog",
            "20260910-090000-000000.tlog",
            "20260909-235900-000000.tlog"
        ]
    );
}

#[test]
fn shutdown_detaches_the_sink_and_is_idempotent() {
    let dir = tmp("shutdown");
    let fake = Fake::new(&[(1, blob(b'x', 90))]);
    let svc = service(&dir, &fake);
    svc.refresh();
    wait_idle(&svc, 10.0);
    svc.shutdown();
    svc.shutdown();
    assert!(fake.detached.load(Ordering::SeqCst));
    assert_eq!(svc.status()["state"], "idle");
}

#[test]
fn listeners_hear_every_state_change() {
    let dir = tmp("listeners");
    let fake = Fake::new(&[(1, blob(b'x', 90))]);
    let svc = service(&dir, &fake);
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let id = {
        let seen = Arc::clone(&seen);
        svc.add_listener(move |v| seen.lock().unwrap().push(v["state"].as_str().unwrap().to_string()))
    };
    svc.refresh();
    wait_idle(&svc, 10.0);
    svc.remove_listener(id);
    assert_eq!(seen.lock().unwrap().as_slice(), ["listing", "idle"]);
    svc.shutdown();
}
