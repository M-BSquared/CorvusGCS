//! One plugin module in a sandbox of its own (wasmtime).
//!
//! The module gets no file system, no sockets, no environment and no
//! arguments: of WASI it has a clock, random numbers and a line to the
//! program's log, and everything else answers "not allowed". Its memory is
//! capped, and a call that runs too long is cut off, so a plugin cannot take
//! the window down or hang it. A trap ends the plugin, never the program.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use corvus_api::ui::filepicker::{self, Filter};
use corvus_api::wire;
use wasmtime::{
    Caller, Config, Engine, Extern, ExternType, Linker, Memory, Module, Store, StoreLimits, StoreLimitsBuilder, Trap,
    TypedFunc, Val, ValType,
};

/// The most memory one plugin may have.
const MEMORY_MAX: usize = 512 * 1024 * 1024;
/// The most a call or a reply may hold.
const MESSAGE_MAX: usize = 96 * 1024 * 1024;
const TICK: Duration = Duration::from_millis(100);
/// How many ticks a call may take: long enough for a slow laptop, short enough
/// that a plugin caught in a loop does not leave the window dead for good.
const CALL_TICKS: u64 = 30;
/// The first call makes fonts and widgets.
const FIRST_TICKS: u64 = 150;
const LOG_LINE: usize = 2000;
const LOG_LINES: u32 = 500;
/// The most of a picked file that is handed over at once.
const FILE_MAX: u64 = 64 * 1024 * 1024;

const ERRNO_BADF: i32 = 8;
const ERRNO_NOTCAPABLE: i32 = 76;
const WASI: &str = "wasi_snapshot_preview1";

/// The engine every module runs on, and the clock that cuts a call off.
pub struct Sandbox {
    engine: Engine,
    stop: Arc<(Mutex<bool>, Condvar)>,
    clock: Option<JoinHandle<()>>,
}

impl Sandbox {
    pub fn new() -> Result<Arc<Sandbox>, String> {
        let mut config = Config::new();
        config.epoch_interruption(true);
        let engine = Engine::new(&config).map_err(|e| format!("the plugin runtime does not start: {e}"))?;
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let (ticking, flag) = (engine.clone(), stop.clone());
        let clock = std::thread::Builder::new()
            .name("plugin-clock".into())
            .spawn(move || {
                let (lock, woken) = &*flag;
                let mut stopped = lock.lock().unwrap_or_else(|p| p.into_inner());
                while !*stopped {
                    stopped = woken.wait_timeout(stopped, TICK).unwrap_or_else(|p| p.into_inner()).0;
                    ticking.increment_epoch();
                }
            })
            .map_err(|e| format!("the plugin runtime does not start: {e}"))?;
        Ok(Arc::new(Sandbox {
            engine,
            stop,
            clock: Some(clock),
        }))
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let (lock, woken) = &*self.stop;
        *lock.lock().unwrap_or_else(|p| p.into_inner()) = true;
        woken.notify_all();
        if let Some(clock) = self.clock.take() {
            let _ = clock.join();
        }
    }
}

/// How the system's file dialog is asked; a test answers by itself.
pub type Picker = Box<dyn FnMut(&wire::PickPath) -> Option<PathBuf>>;

fn system_picker(ask: &wire::PickPath) -> Option<PathBuf> {
    let extensions: Vec<&str> = ask
        .filter
        .iter()
        .flat_map(|(_, e)| e.iter().map(String::as_str))
        .collect();
    let filter = ask.filter.as_ref().map(|(name, _)| Filter {
        name,
        extensions: &extensions,
    });
    let start = ask.start.as_ref().map(PathBuf::from);
    match ask.mode.as_str() {
        "folder" => filepicker::pick_folder(&ask.title, start.as_deref()),
        "save" => filepicker::save_file(&ask.title, &ask.name, filter, start.as_deref()),
        _ => filepicker::pick_file(&ask.title, filter, start.as_deref()),
    }
}

struct State {
    id: String,
    limits: StoreLimits,
    /// What the module said before it trapped.
    panic: Option<String>,
    /// Whether the plugin may ask for files.
    files: bool,
    picker: Picker,
    /// What the operator picked for this plugin: the only files it can read.
    picked: HashSet<PathBuf>,
    logged: u32,
    started: Instant,
}

impl State {
    fn log(&mut self, text: &str) {
        if self.logged < LOG_LINES {
            self.logged += 1;
            let line: String = text.chars().take(LOG_LINE).collect();
            eprintln!("plugin {}: {}", self.id, line.trim_end());
        }
    }

    fn answer(&mut self, query: wire::Query) -> Option<wire::Told> {
        let refused = || wire::Told {
            error: Some("this plugin may not open files".into()),
            ..wire::Told::default()
        };
        match query {
            wire::Query::Panic(line) => {
                self.log(&format!("panic: {}", line.text));
                self.panic = Some(line.text.chars().take(LOG_LINE).collect());
                None
            }
            wire::Query::Log(line) => {
                self.log(&line.text);
                None
            }
            wire::Query::PickPath(_) | wire::Query::ReadPicked(_) if !self.files => Some(refused()),
            wire::Query::PickPath(ask) => {
                let path = (self.picker)(&ask);
                if let Some(p) = &path {
                    self.picked.insert(p.clone());
                }
                Some(wire::Told {
                    path: path.map(|p| p.to_string_lossy().into_owned()),
                    ..wire::Told::default()
                })
            }
            wire::Query::ReadPicked(ask) => {
                let path = PathBuf::from(&ask.path);
                let read = if self.picked.contains(&path) {
                    filepicker::read(&path, ask.max.min(FILE_MAX))
                } else {
                    Err("the operator did not pick this file for the plugin".into())
                };
                Some(match read {
                    Ok(bytes) => wire::Told {
                        bytes: Some(bytes.into()),
                        ..wire::Told::default()
                    },
                    Err(e) => wire::Told {
                        error: Some(e),
                        ..wire::Told::default()
                    },
                })
            }
        }
    }
}

fn memory_of(caller: &mut Caller<'_, State>) -> Option<Memory> {
    caller.get_export(wire::EXPORT_MEMORY).and_then(Extern::into_memory)
}

/// `len` bytes of the module's memory at `ptr`, if they are inside it.
fn read_at(memory: &Memory, store: impl wasmtime::AsContext, ptr: u32, len: u32) -> Option<Vec<u8>> {
    let (ptr, len) = (ptr as usize, len as usize);
    if len > MESSAGE_MAX {
        return None;
    }
    let data = memory.data(&store);
    data.get(ptr..ptr.checked_add(len)?).map(<[u8]>::to_vec)
}

/// A question of the module (`corvus.host`).
fn ask(mut caller: Caller<'_, State>, ptr: u32, len: u32) -> wasmtime::Result<u64> {
    let Some(memory) = memory_of(&mut caller) else {
        return Ok(0);
    };
    let Some(query) = read_at(&memory, &caller, ptr, len).and_then(|b| wire::decode::<wire::Query>(&b).ok()) else {
        return Ok(0);
    };
    let told = caller.data_mut().answer(query);
    // A dialog may have stood open for minutes: that was not the plugin's time.
    caller.as_context_mut_deadline(CALL_TICKS);
    let Some(told) = told else {
        return Ok(0);
    };
    let Ok(bytes) = wire::encode(&told) else {
        return Ok(0);
    };
    let Some(alloc) = caller.get_export(wire::EXPORT_ALLOC).and_then(Extern::into_func) else {
        return Ok(0);
    };
    let alloc = alloc.typed::<u32, u32>(&caller)?;
    let at = alloc.call(&mut caller, bytes.len() as u32)?;
    memory.write(&mut caller, at as usize, &bytes)?;
    Ok((u64::from(at) << 32) | bytes.len() as u64)
}

trait Deadline {
    fn as_context_mut_deadline(&mut self, ticks: u64);
}

impl Deadline for Caller<'_, State> {
    fn as_context_mut_deadline(&mut self, ticks: u64) {
        use wasmtime::AsContextMut;
        self.as_context_mut().set_epoch_deadline(ticks);
    }
}

fn arg(args: &[Val], i: usize) -> u32 {
    args.get(i).and_then(Val::i32).unwrap_or(0) as u32
}

fn put(memory: &Memory, caller: &mut Caller<'_, State>, at: u32, bytes: &[u8]) -> i32 {
    match memory.write(caller, at as usize, bytes) {
        Ok(()) => 0,
        Err(_) => 21, // EFAULT
    }
}

/// The few calls of WASI a module gets a real answer to. `None` for the rest.
fn wasi(name: &str, caller: &mut Caller<'_, State>, args: &[Val]) -> Option<i32> {
    let memory = memory_of(caller)?;
    Some(match name {
        "clock_time_get" => {
            let nanos = if arg(args, 0) == 0 {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos() as u64)
            } else {
                caller.data().started.elapsed().as_nanos() as u64
            };
            put(&memory, caller, arg(args, 2), &nanos.to_le_bytes())
        }
        "clock_res_get" => put(&memory, caller, arg(args, 1), &1000u64.to_le_bytes()),
        "random_get" => {
            let len = arg(args, 1) as usize;
            if len > 1 << 20 {
                return Some(28); // EINVAL
            }
            let mut bytes = vec![0; len];
            if getrandom::fill(&mut bytes).is_err() {
                return Some(29); // EIO
            }
            put(&memory, caller, arg(args, 0), &bytes)
        }
        // No environment and no arguments: two counts of zero.
        "environ_sizes_get" | "args_sizes_get" => {
            let a = put(&memory, caller, arg(args, 0), &0u32.to_le_bytes());
            let b = put(&memory, caller, arg(args, 1), &0u32.to_le_bytes());
            a.max(b)
        }
        "environ_get" | "args_get" | "sched_yield" => 0,
        // What is written to standard out and standard error goes to the log.
        "fd_write" => {
            let fd = arg(args, 0);
            if fd != 1 && fd != 2 {
                return Some(ERRNO_BADF);
            }
            let (list, count) = (arg(args, 1), arg(args, 2).min(64));
            let mut written = 0u32;
            let mut text = Vec::new();
            for i in 0..count {
                let head = read_at(&memory, &*caller, list.saturating_add(i * 8), 8)?;
                let ptr = u32::from_le_bytes([head[0], head[1], head[2], head[3]]);
                let len = u32::from_le_bytes([head[4], head[5], head[6], head[7]]);
                written = written.saturating_add(len);
                if text.len() < LOG_LINE {
                    text.extend(read_at(&memory, &*caller, ptr, len.min(LOG_LINE as u32))?);
                }
            }
            let text = String::from_utf8_lossy(&text).into_owned();
            if !text.trim().is_empty() {
                caller.data_mut().log(&text);
            }
            put(&memory, caller, arg(args, 3), &written.to_le_bytes())
        }
        // No folder was opened for it.
        "fd_prestat_get" | "fd_prestat_dir_name" => ERRNO_BADF,
        _ => return None,
    })
}

fn linker(engine: &Engine, module: &Module) -> Result<Linker<State>, String> {
    let mut linker = Linker::new(engine);
    linker
        .func_wrap(wire::IMPORT_MODULE, wire::IMPORT_HOST, ask)
        .map_err(|e| e.to_string())?;
    for import in module.imports() {
        let (space, name) = (import.module(), import.name().to_string());
        if space == wire::IMPORT_MODULE && name == wire::IMPORT_HOST {
            continue;
        }
        let ExternType::Func(kind) = import.ty() else {
            return Err(format!(
                "it wants {space}.{name} from the program, which has no such thing"
            ));
        };
        if space != WASI {
            return Err(format!(
                "it wants {space}.{name} from the program, which has no such thing"
            ));
        }
        let results: Vec<ValType> = kind.results().collect();
        let call = name.clone();
        linker
            .func_new(space, &name, kind, move |mut caller, args, out| {
                if call == "proc_exit" {
                    return Err(wasmtime::Error::msg("the plugin tried to end the program"));
                }
                let errno = wasi(&call, &mut caller, args).unwrap_or(ERRNO_NOTCAPABLE);
                for (slot, kind) in out.iter_mut().zip(&results) {
                    *slot = match kind {
                        ValType::I64 => Val::I64(0),
                        _ => Val::I32(errno),
                    };
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
    }
    Ok(linker)
}

/// The ending of a compiled module kept beside the module it was made of.
pub const COMPILED: &str = "cwasm";

/// The name of the compiled form of `bytes`: of the module, and of what compiles it, so one made
/// by another version of the runtime or for another processor is never taken.
fn compiled_name(sandbox: &Sandbox, bytes: &[u8]) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    sandbox.engine.precompile_compatibility_hash().hash(&mut hasher);
    let module = corvus_package::sha256_hex(bytes);
    format!("{}-{:016x}.{COMPILED}", &module[..16], hasher.finish())
}

/// `bytes` as machine code. Compiling takes most of a second, so with `cache`, the folder of the
/// installed plugin, the result is kept and the next start reads it. What is kept begins with the
/// checksum of the rest, so a file cut short or damaged is thrown away and made again.
pub fn compiled(sandbox: &Sandbox, bytes: &[u8], cache: Option<&Path>) -> Result<Module, String> {
    let kept = cache.map(|folder| folder.join(compiled_name(sandbox, bytes)));
    if let Some(module) = kept.as_deref().and_then(|p| read_compiled(sandbox, p)) {
        return Ok(module);
    }
    let module = Module::from_binary(&sandbox.engine, bytes)
        .map_err(|e| format!("it is not a module this program can run: {e}"))?;
    if let (Some(path), Ok(code)) = (kept, module.serialize()) {
        let mut file = corvus_package::sha256_hex(&code).into_bytes();
        file.extend_from_slice(&code);
        let part = path.with_extension(format!("{}.tmp", std::process::id()));
        if std::fs::write(&part, file)
            .and_then(|()| std::fs::rename(&part, &path))
            .is_err()
        {
            let _ = std::fs::remove_file(&part);
        }
    }
    Ok(module)
}

fn read_compiled(sandbox: &Sandbox, path: &Path) -> Option<Module> {
    let file = std::fs::read(path).ok()?;
    let (sum, code) = file.split_at_checked(64)?;
    if corvus_package::sha256_hex(code).as_bytes() != sum {
        return None;
    }
    // SAFETY: deserializing trusts that the bytes are what `Module::serialize` gave, since they run
    // as machine code. This program wrote them, into the folder in the operator's home that holds
    // what is installed to run anyway, and the checksum above rules out a damaged file. wasmtime
    // itself refuses one made by another version or for another processor.
    unsafe { Module::deserialize(&sandbox.engine, code) }.ok()
}

/// A running module.
pub struct Instance {
    /// Kept so the clock runs for as long as any module does.
    _sandbox: Arc<Sandbox>,
    store: Store<State>,
    memory: Memory,
    alloc: TypedFunc<u32, u32>,
    call: TypedFunc<(u32, u32), u64>,
    calls: u64,
    dead: bool,
}

impl Instance {
    /// Starts `bytes`, compiled now or taken from `cache` (see [`compiled`]). `files` is whether
    /// it may ask for files.
    pub fn start(
        sandbox: &Arc<Sandbox>,
        id: &str,
        bytes: &[u8],
        files: bool,
        cache: Option<&Path>,
    ) -> Result<Instance, String> {
        let module = compiled(sandbox, bytes, cache)?;
        let linker = linker(&sandbox.engine, &module)?;
        let limits = StoreLimitsBuilder::new()
            .memory_size(MEMORY_MAX)
            .memories(1)
            .instances(1)
            .tables(4)
            .table_elements(1 << 20)
            .build();
        let mut store = Store::new(
            &sandbox.engine,
            State {
                id: id.to_string(),
                limits,
                panic: None,
                files,
                picker: Box::new(system_picker),
                picked: HashSet::new(),
                logged: 0,
                started: Instant::now(),
            },
        );
        store.limiter(|s| &mut s.limits);
        store.epoch_deadline_trap();
        store.set_epoch_deadline(FIRST_TICKS);
        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|e| format!("it could not be started: {e}"))?;
        // A library of WASI sets itself up in `_initialize` before anything else of it runs.
        if let Ok(init) = instance.get_typed_func::<(), ()>(&mut store, "_initialize") {
            init.call(&mut store, ())
                .map_err(|e| format!("it could not be started: {e}"))?;
        }
        let missing = |what: &str| format!("the module has no {what}, so it is not a Corvus plugin");
        let memory = instance
            .get_memory(&mut store, wire::EXPORT_MEMORY)
            .ok_or_else(|| missing("memory"))?;
        let alloc = instance
            .get_typed_func(&mut store, wire::EXPORT_ALLOC)
            .map_err(|_| missing(wire::EXPORT_ALLOC))?;
        let call = instance
            .get_typed_func(&mut store, wire::EXPORT_CALL)
            .map_err(|_| missing(wire::EXPORT_CALL))?;
        Ok(Instance {
            _sandbox: sandbox.clone(),
            store,
            memory,
            alloc,
            call,
            calls: 0,
            dead: false,
        })
    }

    /// Answers the file dialog by itself, for a test.
    #[cfg(test)]
    pub fn set_picker(&mut self, picker: Picker) {
        self.store.data_mut().picker = picker;
    }

    /// One call into the module. An error ends it: nothing more is called.
    pub fn call(&mut self, call: &wire::Call) -> Result<wire::Reply, String> {
        if self.dead {
            return Err("the plugin was stopped".into());
        }
        let result = self.try_call(call);
        if result.is_err() {
            self.dead = true;
        }
        result
    }

    fn try_call(&mut self, call: &wire::Call) -> Result<wire::Reply, String> {
        let bytes = wire::encode(call)?;
        if bytes.len() > MESSAGE_MAX {
            return Err("a call was too large for a plugin".into());
        }
        self.calls += 1;
        let ticks = if self.calls <= 2 { FIRST_TICKS } else { CALL_TICKS };
        self.store.set_epoch_deadline(ticks);
        let run = (|| -> wasmtime::Result<u64> {
            let at = self.alloc.call(&mut self.store, bytes.len() as u32)?;
            self.memory.write(&mut self.store, at as usize, &bytes)?;
            self.call.call(&mut self.store, (at, bytes.len() as u32))
        })();
        let packed = run.map_err(|e| self.why(&e))?;
        let reply = read_at(&self.memory, &self.store, (packed >> 32) as u32, packed as u32)
            .ok_or("the plugin answered outside its memory")?;
        wire::decode(&reply).map_err(|e| format!("the plugin's answer could not be read: {e}"))
    }

    /// A trap in the operator's words.
    fn why(&mut self, error: &wasmtime::Error) -> String {
        if let Some(panic) = self.store.data_mut().panic.take() {
            return format!("the plugin panicked: {panic}");
        }
        match error.downcast_ref::<Trap>() {
            Some(Trap::Interrupt) => "the plugin took too long and was stopped".into(),
            Some(Trap::UnreachableCodeReached) => "the plugin stopped itself, perhaps out of memory".into(),
            Some(trap) => format!("the plugin crashed: {trap}"),
            None => format!("the plugin crashed: {error}"),
        }
    }
}
