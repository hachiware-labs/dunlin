//! Tools, OCR runtimes, user settings and new projects of the CLI (spec 28.13 step 6c), as
//! `project-tools.mjs`, `project-index-tool.mjs`, `parsers/*-setup.mjs`, `project-global.mjs`
//! and `project-modules.mjs` did them.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::cli_project::{CliError, Paths, is_search_project};
use crate::config_value::ConfigValue;
use crate::index_stage::{assign, items, object, string_field, text};
use crate::node_path;
use crate::source_load::sha256_hex;
use crate::wiki_project::{atomic_write, fs_error};

/// Where the CLI runs: the working directory, the home directory and the package files.
#[derive(Clone, Debug, Default)]
pub struct CliEnv {
    pub cwd: String,
    pub home: String,
    /// The `knowledge-nest` package root, which holds `dist/tools` and `tools/rag/parsers`.
    pub package: String,
    /// The `tools` folder of the platform package, when it is installed.
    pub native_tools: Option<String>,
    /// `process.execPath` and the launcher script, for `serve --print-config`.
    pub exec_path: String,
    pub cli: String,
}

impl CliEnv {
    pub fn resolve(&self, path: &str) -> String {
        node_path::resolve(&self.cwd, path)
    }
}

pub fn node_platform() -> &'static str {
    match std::env::consts::OS {
        "windows" => "win32",
        "macos" => "darwin",
        other => other,
    }
}

pub fn node_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        other => other,
    }
}

fn windows() -> bool {
    cfg!(windows)
}

fn coded(exit: u8, code: &str, message: impl Into<String>) -> CliError {
    CliError::new(exit, code, message)
}

fn stage(error: crate::transform_stage::StageError) -> CliError {
    CliError::from(error)
}

// ---------------------------------------------------------------------------------------------
// The catalog.

pub const HANNOY_VERSION: &str = "0.2.0";
pub const OAR_OCR_VERSION: &str = "0.10.2-ppocrv6-small";
pub const MANAGED_OCR_VERSION: &str = "23d544085c886bbdace647bcfab91203299417e2";
pub const LIGHTON_MODEL: &str = "lightonai/LightOnOCR-3-1B";

pub struct Tool {
    pub name: &'static str,
    pub version: &'static str,
    pub modules: &'static [&'static str],
    pub ocr: bool,
    pub legacy: bool,
    pub model: Option<&'static str>,
}

const CATALOG: [Tool; 4] = [
    Tool {
        name: "oar-ocr",
        version: OAR_OCR_VERSION,
        modules: &["loader.ocr"],
        ocr: true,
        legacy: false,
        model: None,
    },
    Tool {
        name: "lightonocr",
        version: MANAGED_OCR_VERSION,
        modules: &["loader.ocr"],
        ocr: true,
        legacy: false,
        model: Some(LIGHTON_MODEL),
    },
    // The former name of lightonocr, still valid in existing projects.
    Tool {
        name: "ocr",
        version: MANAGED_OCR_VERSION,
        modules: &["loader.ocr"],
        ocr: true,
        legacy: true,
        model: Some(LIGHTON_MODEL),
    },
    Tool {
        name: "hannoy",
        version: HANNOY_VERSION,
        modules: &["hannoy.index", "hannoy.search"],
        ocr: false,
        legacy: false,
        model: None,
    },
];

pub fn catalog() -> &'static [Tool] {
    &CATALOG
}

pub fn find(name: &str) -> Option<&'static Tool> {
    CATALOG.iter().find(|tool| tool.name == name)
}

fn strings(values: &[&str]) -> ConfigValue {
    ConfigValue::Array(values.iter().map(|value| text(value)).collect())
}

fn hannoy_configuration() -> ConfigValue {
    object(vec![
        (
            "index",
            object(vec![
                ("use", text("hannoy.index")),
                (
                    "with",
                    object(vec![
                        ("model", text("local-embed")),
                        ("ef_construction", ConfigValue::Number(100.0)),
                    ]),
                ),
            ]),
        ),
        (
            "search",
            object(vec![
                ("use", text("hannoy.search")),
                (
                    "with",
                    object(vec![
                        ("model", text("local-embed")),
                        ("top_k", ConfigValue::Number(10.0)),
                        ("ef_search", ConfigValue::Number(100.0)),
                    ]),
                ),
            ]),
        ),
    ])
}

/// `listedTools()`: the tools that `knowledge-nest add --list` shows.
pub fn listed_tools() -> ConfigValue {
    ConfigValue::Array(
        CATALOG
            .iter()
            .filter(|tool| !tool.legacy)
            .map(|tool| {
                let mut entries = vec![
                    ("name", text(tool.name)),
                    ("version", text(tool.version)),
                    ("modules", strings(tool.modules)),
                ];
                if let Some(model) = tool.model {
                    entries.push(("model", text(model)));
                }
                if tool.name == "hannoy" {
                    entries.push(("configuration", hannoy_configuration()));
                }
                object(entries)
            })
            .collect(),
    )
}

// ---------------------------------------------------------------------------------------------
// The index tool.

const PROTOCOL: f64 = 1.0;

fn binary_name() -> String {
    let suffix = if windows() { ".exe" } else { "" };
    format!(
        "knowledge-nest-hannoy-{}-{}{suffix}",
        node_platform(),
        node_arch()
    )
}

/// Why a check of the index tool failed, as the JavaScript errors told them apart.
enum Failure {
    Coded(CliError),
    Missing,
    Other,
}

struct Manifest {
    file: String,
    sha256: String,
}

fn manifest_at(directory: &str) -> Result<Manifest, Failure> {
    let name = binary_name();
    let path = node_path::join(directory, &[&format!("{name}.json")]);
    let bytes = std::fs::read(&path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => Failure::Missing,
        _ => Failure::Other,
    })?;
    let manifest: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| Failure::Other)?;
    let sha = manifest.get("sha256").and_then(serde_json::Value::as_str);
    let valid = manifest.get("version").and_then(serde_json::Value::as_str) == Some(HANNOY_VERSION)
        && manifest.get("protocol").and_then(serde_json::Value::as_f64) == Some(PROTOCOL)
        && manifest.get("file").and_then(serde_json::Value::as_str) == Some(name.as_str())
        && sha.is_some_and(|sha| {
            sha.len() == 64 && sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        });
    if !valid {
        return Err(Failure::Coded(coded(
            2,
            "TOOL_INTEGRITY",
            "The packaged index tool manifest is invalid.",
        )));
    }
    Ok(Manifest {
        file: name,
        sha256: sha.unwrap_or_default().to_owned(),
    })
}

/// `shippedTool(tool)`: the manifest bundled with the package, or the platform package's.
fn shipped_tool(env: &CliEnv) -> Result<(String, Manifest), Failure> {
    let bundled = node_path::join(&env.package, &["dist", "tools"]);
    match manifest_at(&bundled) {
        Ok(manifest) => return Ok((bundled, manifest)),
        Err(Failure::Missing) => {}
        Err(other) => return Err(other),
    }
    let Some(directory) = env.native_tools.clone() else {
        return Err(Failure::Coded(coded(
            3,
            "TOOL_REQUIRED",
            "The package has no hannoy for this platform.",
        )));
    };
    let manifest = manifest_at(&directory)?;
    Ok((directory, manifest))
}

fn tool_directory(paths: &Paths) -> String {
    node_path::join(
        &paths.data,
        &[
            "tools",
            "hannoy",
            HANNOY_VERSION,
            &format!("{}-{}", node_platform(), node_arch()),
        ],
    )
}

/// `verified(directory, tool, packaged)`: the path and bytes of a binary that passes its checks.
fn verified(
    env: &CliEnv,
    directory: Option<&str>,
) -> Result<(String, Manifest, Vec<u8>), CliError> {
    let checked = (|| {
        let (shipped, manifest) = shipped_tool(env)?;
        let path = node_path::join(directory.unwrap_or(&shipped), &[&binary_name()]);
        let info = std::fs::symlink_metadata(&path).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => Failure::Missing,
            _ => Failure::Other,
        })?;
        if !info.is_file() {
            return Err(Failure::Other);
        }
        let bytes = std::fs::read(&path).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => Failure::Missing,
            _ => Failure::Other,
        })?;
        if sha256_hex(&bytes) != manifest.sha256 {
            return Err(Failure::Other);
        }
        Ok((path, manifest, bytes))
    })();
    checked.map_err(|failure| match failure {
        Failure::Coded(error) => error,
        Failure::Missing => coded(
            3,
            "TOOL_REQUIRED",
            "The hannoy binary is missing. Run knowledge-nest add hannoy.",
        ),
        Failure::Other => coded(
            2,
            "TOOL_INTEGRITY",
            "The hannoy binary failed its version or integrity check.",
        ),
    })
}

/// `checkIndexTool(paths, hannoy)`
pub fn check_index_tool(env: &CliEnv, paths: &Paths) -> Result<(), CliError> {
    verified(env, Some(&tool_directory(paths))).map(|_| ())
}

/// `indexToolPath(paths, hannoy)`: the installed tool after its checks.
pub fn index_tool_path(env: &CliEnv, paths: &Paths) -> Result<String, CliError> {
    verified(env, Some(&tool_directory(paths))).map(|(path, ..)| path)
}

/// `installIndexTool(project, tool)`: copies the packaged tool into the project and checks it.
fn install_index_tool(env: &CliEnv, paths: &Paths) -> Result<(), CliError> {
    let (_, manifest, bytes) = verified(env, None)?;
    let directory = tool_directory(paths);
    std::fs::create_dir_all(&directory)
        .map_err(|error| stage(fs_error("mkdir", &directory, &error)))?;
    let path = node_path::join(&directory, &[&manifest.file]);
    atomic_write(&path, &bytes).map_err(stage)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .map_err(|error| stage(fs_error("chmod", &path, &error)))?;
    }
    let path = index_tool_path(env, paths)?;
    let description = crate::index_stage::call_tool(
        &path,
        "{\"operation\":\"describe\"}",
        "describe",
        &crate::Cancellation::default(),
    )
    .map_err(stage)?;
    let modules = description.get("modules").map(serde_json::Value::to_string);
    let capabilities = description
        .get("capabilities")
        .and_then(serde_json::Value::as_array);
    let valid = description
        .get("protocol")
        .and_then(serde_json::Value::as_f64)
        == Some(PROTOCOL)
        && description.get("name").and_then(serde_json::Value::as_str) == Some("hannoy")
        && description
            .get("version")
            .and_then(serde_json::Value::as_str)
            == Some(HANNOY_VERSION)
        && modules.as_deref() == Some("[\"hannoy.index\",\"hannoy.search\"]")
        && capabilities.is_some_and(|list| {
            ["build", "search"]
                .iter()
                .all(|value| list.iter().any(|item| item.as_str() == Some(value)))
        });
    if !valid {
        return Err(coded(
            2,
            "TOOL_PROTOCOL",
            "The installed index tool uses an incompatible protocol.",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// OCR runtimes.

/// `dataRoot()`: where the managed OCR runtime and models live.
pub fn data_root(env: &CliEnv) -> Result<String, CliError> {
    if let Some(value) = std::env::var_os("KNOWLEDGE_NEST_OCR_HOME") {
        let value = value.to_string_lossy().into_owned();
        if !node_path::is_absolute(&value) {
            return Err(CliError::plain(
                "KNOWLEDGE_NEST_OCR_HOME must be an absolute path",
            ));
        }
        return Ok(value);
    }
    let absolute = |name: &str| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.is_empty() && node_path::is_absolute(value))
    };
    Ok(match node_platform() {
        "win32" => {
            let local = absolute("LOCALAPPDATA")
                .unwrap_or_else(|| node_path::join(&env.home, &["AppData", "Local"]));
            node_path::join(&local, &["Knowledge-Nest", "ocr"])
        }
        "darwin" => node_path::join(
            &env.home,
            &["Library", "Application Support", "Knowledge-Nest", "ocr"],
        ),
        _ => {
            let data = absolute("XDG_DATA_HOME")
                .unwrap_or_else(|| node_path::join(&env.home, &[".local", "share"]));
            node_path::join(&data, &["knowledge-nest", "ocr"])
        }
    })
}

/// `oarOcrHome()`: the folder of oar-ocr, beside the lightonocr folder.
pub fn oar_ocr_home(env: &CliEnv) -> Result<String, CliError> {
    Ok(node_path::join(
        &node_path::dirname(&data_root(env)?),
        &["oar-ocr", OAR_OCR_VERSION],
    ))
}

fn python_path(runtime: &str) -> String {
    if windows() {
        node_path::join(runtime, &["venv", "Scripts", "python.exe"])
    } else {
        node_path::join(runtime, &["venv", "bin", "python"])
    }
}

fn model_path(root: &str) -> String {
    node_path::join(root, &[&format!("model-{MANAGED_OCR_VERSION}")])
}

const MODEL_FILES: [(&str, u64, &str); 8] = [
    (
        "chat_template.jinja",
        720,
        "0942a43ee0ba48a608814e0b40ac9cb70bcc29ea72f19c84b1bf2597f05aa2bd",
    ),
    (
        "config.json",
        2363,
        "43b62f9ec0a7207f67fd032f7ef9719fa8b6b44d7b3cf20fbc2346329493ff3a",
    ),
    (
        "generation_config.json",
        219,
        "7d2566b271454af41e9ebc28851888e1ec7ca33a53fe644dac207856df952981",
    ),
    (
        "model.safetensors",
        2_011_367_464,
        "32879861d5cdc3b2c3909d3f9a47dc8f7338685d13113c8c98c144f68b358dbb",
    ),
    (
        "processor_config.json",
        1023,
        "5292a61a089835ec6884836e3896bba1fc571c60df872e3ca9eb45bd3288910d",
    ),
    (
        "special_tokens_map.json",
        613,
        "76862e765266b85aa9459767e33cbaf13970f327a0e88d1c65846c2ddd3a1ecd",
    ),
    (
        "tokenizer_config.json",
        5591,
        "76d82c8010d0e0d5431aa5e3658c69e11a2a7250e789799d2be4ab75dc3fafed",
    ),
    (
        "tokenizer.json",
        11_422_822,
        "f54b55fa0c3aba0c91ce09ea79ae4e62da24e2a1a630f96c4bae34aba25e234a",
    ),
];

fn platform_key() -> String {
    format!("{}/{}", node_platform(), node_arch())
}

fn backends() -> &'static [&'static str] {
    if platform_key() == "darwin/arm64" {
        &["mps"]
    } else {
        &["cuda", "rocm"]
    }
}

struct Runtime {
    backend: String,
    python: String,
}

/// `installedRuntime(root)`: the runtime `runtime.json` names, when it is usable.
fn installed_runtime(root: &str) -> Result<Option<Runtime>, CliError> {
    let path = node_path::join(root, &["runtime.json"]);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(stage(fs_error("open", &path, &error))),
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Ok(None);
    };
    let field = |name: &str| value.get(name).and_then(serde_json::Value::as_str);
    let directory = field("directory").unwrap_or_default();
    let valid = value.get("version").and_then(serde_json::Value::as_f64) == Some(1.0)
        && field("platform") == Some(platform_key().as_str())
        && field("backend").is_some_and(|backend| backends().contains(&backend))
        && directory.len() == 9
        && directory.starts_with("rt-")
        && directory[3..].bytes().all(|b| b.is_ascii_alphanumeric());
    if !valid {
        return Ok(None);
    }
    let python = python_path(&node_path::join(root, &[directory]));
    match std::fs::metadata(&python) {
        Ok(info) if info.is_file() => Ok(Some(Runtime {
            backend: field("backend").unwrap_or_default().to_owned(),
            python,
        })),
        Ok(_) => Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(stage(fs_error("stat", &python, &error))),
    }
}

/// `managedOcr({ required: false })`: the prepared runtime and model, or none.
fn managed_ocr(env: &CliEnv) -> Result<Option<(String, String)>, CliError> {
    let root = data_root(env)?;
    let Some(runtime) = installed_runtime(&root)? else {
        return Ok(None);
    };
    let model_dir = model_path(&root);
    for (name, size, _) in MODEL_FILES {
        let path = node_path::join(&model_dir, &[name]);
        match std::fs::metadata(&path) {
            Ok(info) if info.is_file() && info.len() == size => {}
            Ok(_) => return Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(stage(fs_error("stat", &path, &error))),
        }
    }
    Ok(Some((runtime.python, model_dir)))
}

pub fn lighton_server(env: &CliEnv) -> String {
    node_path::join(
        &env.package,
        &["tools", "rag", "parsers", "lighton-server.py"],
    )
}

/// `ocrRequest(projectOcr(tools))`: the OCR fields of a load request.
pub fn ocr_request(env: &CliEnv, tools: Option<&ConfigValue>) -> Result<ConfigValue, CliError> {
    let ocr = crate::cli_project::project_ocr(tools);
    if let Some((tool, version)) = &ocr
        && tool == "oar-ocr"
    {
        return Ok(object(vec![
            (
                "ocrVersion",
                text(&format!(
                    "oar-ocr@{}",
                    crate::eval_stage::js_string(Some(version))
                )),
            ),
            ("ocrTool", text("oar-ocr")),
            ("ocrHome", text(&oar_ocr_home(env)?)),
        ]));
    }
    let mut entries = vec![
        ("ocrVersion", text(MANAGED_OCR_VERSION)),
        ("ocrTool", text("lightonocr")),
    ];
    if ocr.is_some()
        && let Some((python, model_dir)) = managed_ocr(env)?
    {
        entries.push((
            "lighton",
            object(vec![
                ("python", text(&python)),
                ("server", text(&lighton_server(env))),
                ("modelDir", text(&model_dir)),
                ("model", text(LIGHTON_MODEL)),
            ]),
        ));
    }
    Ok(object(entries))
}

/// The HTTP client of downloads: fetch's defaults, which follow redirects and ignore proxies.
fn download_agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .proxy(None)
        .http_status_as_error(false)
        .build()
        .into()
}

fn file_digest(path: &str) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(to_hex(&digest.finalize()))
}

/// `downloadVerified(url, target, checksum, timeout)`: a file kept when it matches its checksum.
fn download_verified(
    url: &str,
    target: &str,
    checksum: &str,
    timeout: Duration,
) -> Result<(), CliError> {
    use sha2::{Digest, Sha256};
    use std::io::Write;
    match file_digest(target) {
        Ok(existing) if existing == checksum => return Ok(()),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(stage(fs_error("open", target, &error))),
    }
    let folder = node_path::dirname(target);
    std::fs::create_dir_all(&folder).map_err(|error| stage(fs_error("mkdir", &folder, &error)))?;
    let temporary = format!("{target}.{}.tmp", uuid::Uuid::new_v4());
    let result = (|| {
        let mut response = download_agent(timeout)
            .get(url)
            .call()
            .map_err(|error| CliError::plain(format!("Could not download {url}: {error}")))?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(CliError::plain(format!(
                "Could not download {url}: HTTP {status}"
            )));
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| stage(fs_error("open", &temporary, &error)))?;
        let mut reader = response.body_mut().with_config().limit(u64::MAX).reader();
        let mut digest = Sha256::new();
        let mut buffer = vec![0; 1 << 20];
        loop {
            let read = reader
                .read(&mut buffer)
                .map_err(|error| CliError::plain(format!("Could not download {url}: {error}")))?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
            file.write_all(&buffer[..read])
                .map_err(|error| stage(fs_error("write", &temporary, &error)))?;
        }
        drop(file);
        if to_hex(&digest.finalize()) != checksum {
            return Err(CliError::plain(format!("SHA-256 mismatch for {url}")));
        }
        std::fs::rename(&temporary, target)
            .map_err(|error| stage(fs_error("rename", &temporary, &error)))
    })();
    let _ = std::fs::remove_file(&temporary);
    result
}

fn tar() -> String {
    match std::env::var("SystemRoot") {
        Ok(root) if windows() && !root.is_empty() => {
            node_path::join(&root, &["System32", "tar.exe"])
        }
        _ => "tar".into(),
    }
}

fn hidden(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command
}

/// Runs a setup step with its output on standard error.
fn run(
    executable: &str,
    args: &[String],
    failure: impl Fn(String) -> String,
) -> Result<(), CliError> {
    let status = hidden(
        Command::new(executable)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::from(std::io::stderr()))
            .stderr(Stdio::from(std::io::stderr())),
    )
    .status()
    .map_err(|error| CliError::plain(format!("spawn {executable} {error}")))?;
    if status.success() {
        return Ok(());
    }
    let code = status
        .code()
        .map_or_else(|| "null".into(), |code| code.to_string());
    Err(CliError::plain(failure(code)))
}

/// `execFile(executable, args, { timeout })`: the standard output, or the error and its output.
fn exec(executable: &str, args: &[&str], timeout: Duration) -> Result<String, (String, String)> {
    let mut child = hidden(
        Command::new(executable)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .spawn()
    .map_err(|error| (format!("spawn {executable} {error}"), String::new()))?;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let out = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(pipe) = stdout.as_mut() {
            let _ = pipe.read_to_string(&mut text);
        }
        text
    });
    let err = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(pipe) = stderr.as_mut() {
            let _ = pipe.read_to_string(&mut text);
        }
        text
    });
    let started = Instant::now();
    let status = loop {
        if let Ok(Some(status)) = child.try_wait() {
            break Some(status);
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    match status {
        Some(status) if status.success() => Ok(stdout),
        _ => Err((
            format!("Command failed: {executable} {}", args.join(" ")),
            stderr,
        )),
    }
}

fn probe_gpu(env: &CliEnv, python: &str, backend: &str) -> Result<ConfigValue, CliError> {
    let server = lighton_server(env);
    let failed = |detail: String| {
        CliError::plain(format!(
            "GPU check failed for {backend}: {detail}. Check the vendor driver and GPU compatibility."
        ))
    };
    let stdout = exec(python, &["-I", &server, "--probe"], Duration::from_secs(60)).map_err(
        |(message, stderr)| {
            failed(
                stderr
                    .trim()
                    .lines()
                    .last()
                    .filter(|line| !line.is_empty())
                    .map_or(message, str::to_owned),
            )
        },
    )?;
    let value: ConfigValue =
        serde_json::from_str(&stdout).map_err(|error| failed(format!("{error}")))?;
    let runtime = string_field(&value, "runtime").unwrap_or_default();
    if runtime.to_lowercase() != backend {
        return Err(failed(format!("Expected {backend}, got {runtime}")));
    }
    Ok(object(vec![
        ("device", value.get("device").cloned().unwrap_or_default()),
        (
            "deviceName",
            value.get("deviceName").cloned().unwrap_or_default(),
        ),
        ("runtime", value.get("runtime").cloned().unwrap_or_default()),
        ("dtype", value.get("dtype").cloned().unwrap_or_default()),
    ]))
}

fn detect_backend() -> Result<&'static str, CliError> {
    match node_platform() {
        "darwin" => return Ok("mps"),
        "win32" => {
            let stdout = exec(
                "powershell.exe",
                &[
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "(Get-CimInstance Win32_VideoController).PNPDeviceID",
                ],
                Duration::from_secs(15),
            )
            .map_err(|(message, _)| CliError::plain(message))?
            .to_uppercase();
            if stdout.contains("VEN_10DE") {
                return Ok("cuda");
            }
            if stdout.contains("VEN_1002") {
                return Ok("rocm");
            }
        }
        _ => {
            let mut vendors = Vec::new();
            if let Ok(cards) = std::fs::read_dir("/sys/class/drm") {
                for card in cards.flatten() {
                    let name = card.file_name().to_string_lossy().into_owned();
                    if name.len() > 4
                        && name.starts_with("card")
                        && name[4..].bytes().all(|b| b.is_ascii_digit())
                    {
                        vendors.push(
                            std::fs::read_to_string(format!("/sys/class/drm/{name}/device/vendor"))
                                .unwrap_or_default()
                                .trim()
                                .to_owned(),
                        );
                    }
                }
            }
            if vendors.iter().any(|vendor| vendor == "0x10de") {
                return Ok("cuda");
            }
            if exec("nvidia-smi", &["-L"], Duration::from_secs(5))
                .is_ok_and(|out| out.contains("GPU "))
            {
                return Ok("cuda");
            }
            if vendors.iter().any(|vendor| vendor == "0x1002") {
                return Ok("rocm");
            }
        }
    }
    Err(CliError::plain(
        "Could not identify an NVIDIA or AMD GPU. Check the vendor driver or specify --backend cuda or --backend rocm.",
    ))
}

const UV_RELEASE: &str = "https://releases.astral.sh/github/uv/releases/download/0.13.7";
const PYTHON_RELEASE: &str =
    "https://github.com/astral-sh/python-build-standalone/releases/download/20261026";
const ROCM_WINDOWS: &str = "https://repo.radeon.com/rocm/windows/rocm-rel-7.2.1";
const ROCM_LINUX: &str = "https://repo.radeon.com/rocm/manylinux/rocm-rel-7.2.1";

fn uv_asset() -> Option<(&'static str, &'static str)> {
    match platform_key().as_str() {
        "win32/x64" => Some((
            "uv-x86_64-pc-windows-msvc.zip",
            "0cdc5b38fe5a8168ed6a25814f5ba43c21d313dd3187314fccb37b7cec46e021",
        )),
        "linux/x64" => Some((
            "uv-x86_64-unknown-linux-gnu.tar.gz",
            "c5221ff41b7892bfc223543883600125f6a47fe845093e0f5bdff4f359196d83",
        )),
        "darwin/arm64" => Some((
            "uv-aarch64-apple-darwin.tar.gz",
            "81b811b57e4624df7ed3aac148e0359100d9962f3d9dce00aec533dc2e086731",
        )),
        _ => None,
    }
}

fn python_asset() -> Option<(&'static str, &'static str)> {
    match platform_key().as_str() {
        "win32/x64" => Some((
            "x86_64-pc-windows-msvc",
            "1f2ab53880fc67ea323398c6388e05af28addc99a8e389209f08a51fdff4b149",
        )),
        "linux/x64" => Some((
            "x86_64-unknown-linux-gnu",
            "ce4f6d0a6cfdb3bed6e44c2f9affd85943c76a42e25ad3a259002f31d6fb8a20",
        )),
        "darwin/arm64" => Some((
            "aarch64-apple-darwin",
            "353d5f1b4fd2715e7053f12f068435d5bbad16b91e057006b5d01e0925709a7e",
        )),
        _ => None,
    }
}

fn base_name(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_owned()
}

fn extract_release(
    release: &str,
    asset: &str,
    checksum: &str,
    runtime: &str,
) -> Result<(), CliError> {
    let archive = node_path::join(runtime, &[asset]);
    let encoded: String = url::form_urlencoded::byte_serialize(asset.as_bytes())
        .collect::<String>()
        .replace('+', "%20");
    download_verified(
        &format!("{release}/{encoded}"),
        &archive,
        checksum,
        Duration::from_secs(180),
    )?;
    let tar = tar();
    let args = vec![
        "-xf".to_owned(),
        archive.clone(),
        "-C".to_owned(),
        runtime.to_owned(),
    ];
    run(&tar, &args, |code| {
        format!(
            "{} -xf failed ({code}). See setup output above.",
            base_name(&tar)
        )
    })?;
    std::fs::remove_file(&archive).map_err(|error| stage(fs_error("rm", &archive, &error)))
}

fn install_runtime(runtime: &str, backend: &str) -> Result<String, CliError> {
    let (asset, uv_checksum) = uv_asset().unwrap_or_default();
    let (target, python_checksum) = python_asset().unwrap_or_default();
    extract_release(UV_RELEASE, asset, uv_checksum, runtime)?;
    extract_release(
        PYTHON_RELEASE,
        &format!("cpython-3.12.16+20261026-{target}-install_only_stripped.tar.gz"),
        python_checksum,
        runtime,
    )?;
    let uv = if windows() {
        node_path::join(runtime, &["uv.exe"])
    } else {
        node_path::join(runtime, &[asset.trim_end_matches(".tar.gz"), "uv"])
    };
    let base_python = if windows() {
        node_path::join(runtime, &["python", "python.exe"])
    } else {
        node_path::join(runtime, &["python", "bin", "python3.12"])
    };
    let python = python_path(runtime);
    let uv_failure = |args: &[String]| {
        let first = args.first().cloned().unwrap_or_default();
        let name = base_name(&uv);
        move |code: String| format!("{name} {first} failed ({code}). See setup output above.")
    };
    let venv: Vec<String> = [
        "venv",
        "--no-config",
        "--no-python-downloads",
        "--python",
        &base_python,
        &node_path::join(runtime, &["venv"]),
    ]
    .iter()
    .map(|value| (*value).to_owned())
    .collect();
    run(&uv, &venv, uv_failure(&venv))?;
    let pip = |extra: Vec<String>| {
        let mut args: Vec<String> = ["pip", "install", "--no-config", "--python", &python]
            .iter()
            .map(|value| (*value).to_owned())
            .collect();
        args.extend(extra);
        run(&uv, &args, uv_failure(&args))
    };
    let files = |base: &str, names: &[&str]| -> Vec<String> {
        names.iter().map(|file| format!("{base}/{file}")).collect()
    };
    let torch = vec!["torch==2.9.1".to_owned(), "torchvision==0.24.1".to_owned()];
    if backend == "rocm" && windows() {
        pip(files(
            ROCM_WINDOWS,
            &[
                "rocm_sdk_core-7.2.1-py3-none-win_amd64.whl",
                "rocm_sdk_devel-7.2.1-py3-none-win_amd64.whl",
                "rocm_sdk_libraries_custom-7.2.1-py3-none-win_amd64.whl",
                "rocm-7.2.1.tar.gz",
                "torch-2.9.1%2Brocm7.2.1-cp312-cp312-win_amd64.whl",
                "torchvision-0.24.1%2Brocm7.2.1-cp312-cp312-win_amd64.whl",
            ],
        ))?;
    } else if backend == "rocm" {
        pip(files(
            ROCM_LINUX,
            &[
                "torch-2.9.1%2Brocm7.2.1.lw.gitff65f5bc-cp312-cp312-linux_x86_64.whl",
                "torchvision-0.24.0%2Brocm7.2.1.gitb919bd0c-cp312-cp312-linux_x86_64.whl",
                "triton-3.5.1%2Brocm7.2.1.gita272dfa8-cp312-cp312-linux_x86_64.whl",
            ],
        ))?;
    } else if backend == "cuda" {
        let mut args = vec![
            "--index-url".to_owned(),
            "https://download.pytorch.org/whl/cu128".to_owned(),
        ];
        args.extend(torch);
        pip(args)?;
    } else {
        pip(torch)?;
    }
    pip(vec!["transformers==5.16.1".into(), "Pillow".into()])?;
    Ok(python)
}

/// `setupManagedOcr()`: the lightonocr runtime and model, prepared once.
fn setup_managed_ocr(env: &CliEnv) -> Result<(), CliError> {
    let platform = platform_key();
    if uv_asset().is_none() {
        return Err(CliError::plain(format!(
            "Managed GPU OCR is unavailable on {platform}. Use oar-ocr instead: knowledge-nest add oar-ocr."
        )));
    }
    let root = data_root(env)?;
    let existing = installed_runtime(&root)?;
    let mut ready = existing
        .as_ref()
        .is_some_and(|runtime| probe_gpu(env, &runtime.python, &runtime.backend).is_ok());
    let backend = match (&existing, ready) {
        (Some(runtime), true) => runtime.backend.clone(),
        _ => detect_backend()?.to_owned(),
    };
    std::fs::create_dir_all(&root).map_err(|error| stage(fs_error("mkdir", &root, &error)))?;
    let runtime = if ready {
        None
    } else {
        let suffix: String = uuid::Uuid::new_v4().simple().to_string()[..6].to_owned();
        let directory = node_path::join(&root, &[&format!("rt-{suffix}")]);
        std::fs::create_dir(&directory)
            .map_err(|error| stage(fs_error("mkdtemp", &directory, &error)))?;
        Some(directory)
    };
    let temporary = node_path::join(&root, &[&format!("runtime-{}.tmp", uuid::Uuid::new_v4())]);
    let mut published = false;
    let result = (|| {
        if let Some(runtime) = &runtime {
            eprintln!("Preparing the {backend} OCR runtime in {root}");
            let python = install_runtime(runtime, &backend)?;
            probe_gpu(env, &python, &backend)?;
            ready = true;
        }
        eprintln!(
            "Preparing and verifying {LIGHTON_MODEL} in {}",
            model_path(&root)
        );
        let model_url =
            format!("https://huggingface.co/{LIGHTON_MODEL}/resolve/{MANAGED_OCR_VERSION}");
        let downloads: Vec<_> = MODEL_FILES
            .iter()
            .map(|(name, _, checksum)| {
                let url = format!("{model_url}/{name}");
                let target = node_path::join(&model_path(&root), &[name]);
                let checksum = (*checksum).to_owned();
                std::thread::spawn(move || {
                    download_verified(&url, &target, &checksum, Duration::from_secs(7200))
                })
            })
            .collect();
        let mut failure = None;
        for download in downloads {
            if let Ok(Err(error)) = download.join()
                && failure.is_none()
            {
                failure = Some(error);
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        if let Some(runtime) = &runtime {
            let record = object(vec![
                ("version", ConfigValue::Number(1.0)),
                ("platform", text(&platform)),
                ("backend", text(&backend)),
                ("directory", text(&base_name(runtime))),
            ]);
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .and_then(|mut file| {
                    std::io::Write::write_all(&mut file, record.to_js_json().as_bytes())
                })
                .map_err(|error| stage(fs_error("open", &temporary, &error)))?;
            let target = node_path::join(&root, &["runtime.json"]);
            std::fs::rename(&temporary, &target)
                .map_err(|error| stage(fs_error("rename", &temporary, &error)))?;
            published = true;
        }
        Ok(())
    })();
    let _ = std::fs::remove_file(&temporary);
    if let Some(runtime) = &runtime
        && !published
    {
        let _ = std::fs::remove_dir_all(runtime);
    }
    let _ = ready;
    result
}

const ONNX_VERSION: &str = "1.28.3";
const OAR_MODELS: [(&str, &str, &str); 3] = [
    (
        "detection",
        "pp-ocrv6_small_det.onnx",
        "d73e0058b7a8086bbd57f3d10b8bcd4ff95363f67e06e2762b5e814fe9c9410e",
    ),
    (
        "recognition",
        "pp-ocrv6_small_rec.onnx",
        "5435fd747c9e0efe15a96d0b378d5bd157e9492ed8fd80edf08f30d02fa24634",
    ),
    (
        "dictionary",
        "ppocrv6_dict.txt",
        "b5f2bfe2bdd9448429e3e82b51c789775d9b42f2403d082b00662eb77e401c5d",
    ),
];

fn onnx_runtime() -> Option<(&'static str, &'static str, &'static str)> {
    match platform_key().as_str() {
        "win32/x64" => Some((
            "onnxruntime-win-x64",
            "zip",
            "1d6fab48e85f948436af7c8c971d2c145cf224e2c444755dec894f8b0de11a83",
        )),
        "win32/arm64" => Some((
            "onnxruntime-win-arm64",
            "zip",
            "76d5e5a23fb7fdbc2ca0c3214bad1b58b2a41016c7052e74e60ed8004bea71b1",
        )),
        "linux/x64" => Some((
            "onnxruntime-linux-x64",
            "tgz",
            "db14e4863bd37893fc59729d986ab2a0d043d10b7d44da1913c4982b7e3d009c",
        )),
        "linux/arm64" => Some((
            "onnxruntime-linux-aarch64",
            "tgz",
            "6c6b1ae96d7b0be9f555092857c6a4f2b0b5587a5298d732d9e52e8253038ce7",
        )),
        "darwin/arm64" => Some((
            "onnxruntime-osx-arm64",
            "tgz",
            "c436bd9f47dbce6f6311ccc21de97f829b3c862c09d24aa321958cb72f0a1d32",
        )),
        _ => None,
    }
}

fn runtime_library(lib: &str) -> Result<String, CliError> {
    let names: Vec<String> = std::fs::read_dir(lib)
        .map_err(|error| stage(fs_error("scandir", lib, &error)))?
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    let numbered = |name: &str, prefix: &str, suffix: &str| {
        name.strip_prefix(prefix)
            .and_then(|rest| rest.strip_suffix(suffix))
            .is_some_and(|version| {
                let parts: Vec<&str> = version.split('.').collect();
                parts.len() == 3
                    && parts
                        .iter()
                        .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
            })
    };
    let name = names.into_iter().find(|name| match node_platform() {
        "win32" => name == "onnxruntime.dll",
        "darwin" => numbered(name, "libonnxruntime.", ".dylib"),
        _ => numbered(name, "libonnxruntime.so.", ""),
    });
    name.map(|name| node_path::join(lib, &[&name]))
        .ok_or_else(|| {
            CliError::plain("The ONNX Runtime release has no library for this platform.")
        })
}

/// `setupOarOcr()`: ONNX Runtime and the models, downloaded and checked once.
fn setup_oar_ocr(env: &CliEnv) -> Result<(), CliError> {
    let Some((name, format, checksum)) = onnx_runtime() else {
        let mut error =
            CliError::plain(format!("oar-ocr is not available for {}.", platform_key()));
        error.code = Some("OCR_NOT_PREPARED".into());
        return Err(error);
    };
    let home = oar_ocr_home(env)?;
    let manifest = node_path::join(&home, &["oar-ocr.json"]);
    if std::fs::metadata(&manifest).is_ok() {
        return Ok(());
    }
    std::fs::create_dir_all(&home).map_err(|error| stage(fs_error("mkdir", &home, &error)))?;
    let models = "https://github.com/GreatV/oar-ocr/releases/download/v0.7.0";
    for (_, file, sum) in OAR_MODELS {
        download_verified(
            &format!("{models}/{file}"),
            &node_path::join(&home, &[file]),
            sum,
            Duration::from_secs(600),
        )?;
    }
    let folder = format!("{name}-{ONNX_VERSION}");
    let archive = node_path::join(&home, &[&format!("{folder}.{format}")]);
    download_verified(
        &format!(
            "https://github.com/microsoft/onnxruntime/releases/download/v{ONNX_VERSION}/{folder}.{format}"
        ),
        &archive,
        checksum,
        Duration::from_secs(600),
    )?;
    let staging = node_path::join(&home, &["runtime.tmp"]);
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir(&staging).map_err(|error| stage(fs_error("mkdir", &staging, &error)))?;
    let moved = (|| {
        let args = vec![
            "-xf".to_owned(),
            archive.clone(),
            "-C".to_owned(),
            staging.clone(),
        ];
        run(&tar(), &args, |code| {
            format!("Could not extract {archive} ({code}).")
        })?;
        let target = node_path::join(&home, &["runtime"]);
        let _ = std::fs::remove_dir_all(&target);
        let source = node_path::join(&staging, &[&folder]);
        std::fs::rename(&source, &target)
            .map_err(|error| stage(fs_error("rename", &source, &error)))
    })();
    let _ = std::fs::remove_dir_all(&staging);
    let _ = std::fs::remove_file(&archive);
    moved?;
    let library = runtime_library(&node_path::join(&home, &["runtime", "lib"]))?;
    let mut entries = vec![
        ("version", text(OAR_OCR_VERSION)),
        (
            "runtime",
            text(&node_path::relative(&home, &library).replace('\\', "/")),
        ),
    ];
    for (role, file, _) in OAR_MODELS {
        entries.push((role, text(file)));
    }
    let json = crate::cli_project::pretty_json(&object(entries)) + "\n";
    std::fs::write(&manifest, json).map_err(|error| stage(fs_error("open", &manifest, &error)))
}

// ---------------------------------------------------------------------------------------------
// Adding and removing tools.

/// `withWriteLock(root, write)`: `.knowledge-nest-write.lock` held while `write` runs.
pub fn with_write_lock<T>(
    root: &str,
    write: impl FnOnce() -> Result<T, CliError>,
) -> Result<T, CliError> {
    let path = node_path::join(root, &[".knowledge-nest-write.lock"]);
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path);
    match lock {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(CliError::plain(format!(
                "Another writer is updating {root}"
            )));
        }
        Err(error) => return Err(stage(fs_error("open", &path, &error))),
    }
    let result = write();
    let _ = std::fs::remove_file(&path);
    result
}

fn read_utf8(path: &str) -> Result<String, CliError> {
    crate::wiki_project::read_file(path).map_err(stage)
}

/// `changeTool(project, requested, remove)`
pub fn change_tool(
    env: &CliEnv,
    project: &crate::cli_project::Project,
    requested: &str,
    remove: bool,
) -> Result<ConfigValue, CliError> {
    let tools = project.config.get("tools");
    let recorded = items(tools).iter().find(|entry| {
        string_field(entry, "name")
            .and_then(find)
            .is_some_and(|tool| tool.ocr)
    });
    let recorded_name = recorded.and_then(|entry| string_field(entry, "name"));
    let name = if requested == "ocr" {
        recorded_name.unwrap_or(if remove { "ocr" } else { "oar-ocr" })
    } else {
        requested
    };
    let Some(tool) = find(name) else {
        return Err(coded(
            2,
            "TOOL_UNKNOWN",
            "Use knowledge-nest add --list to see the available tools.",
        ));
    };
    if !remove
        && tool.ocr
        && let Some(recorded) = recorded_name
        && recorded != name
        && !(name == "lightonocr" && recorded == "ocr")
    {
        return Err(coded(
            2,
            "TOOL_CONFLICT",
            format!("A project uses one OCR tool. Run knowledge-nest remove {recorded} first."),
        ));
    }
    let kind = project
        .config
        .get("init")
        .and_then(|init| string_field(init, "type"))
        .unwrap_or_default();
    if name == "hannoy" && !is_search_project(kind) {
        return Err(coded(
            2,
            "WRONG_PROJECT_TYPE",
            "The hannoy tool requires a RAG/MCP project.",
        ));
    }
    if remove
        && ["source", "ingress", "index", "search"]
            .iter()
            .any(|section| {
                items(
                    project
                        .config
                        .get(section)
                        .and_then(|section| section.get("modules")),
                )
                .iter()
                .any(|entry| {
                    string_field(entry, "use").is_some_and(|name| tool.modules.contains(&name))
                        || (tool.ocr
                            && entry.get("with").and_then(|args| string_field(args, "ocr"))
                                == Some("loader.ocr"))
                })
            })
    {
        return Err(coded(
            2,
            "TOOL_IN_USE",
            format!("Remove references to {name} modules from the KnowledgeNestfile first."),
        ));
    }
    if !remove {
        if name == "oar-ocr" {
            setup_oar_ocr(env)?;
        } else if tool.ocr {
            setup_managed_ocr(env)?;
        } else {
            install_index_tool(env, &project.paths)?;
        }
    }
    let mut config = match &project.config {
        ConfigValue::Object(entries) => entries.clone(),
        _ => Vec::new(),
    };
    let mut list: Vec<ConfigValue> = items(tools)
        .iter()
        .filter(|entry| {
            let entry_name = string_field(entry, "name");
            entry_name != Some(name) && !(name == "lightonocr" && entry_name == Some("ocr"))
        })
        .cloned()
        .collect();
    if !remove {
        list.push(object(vec![
            ("name", text(name)),
            ("version", text(tool.version)),
        ]));
    }
    assign(
        &mut config,
        "tools",
        (!list.is_empty()).then_some(ConfigValue::Array(list)),
    );
    let config = ConfigValue::Object(config);
    with_write_lock(&project.paths.data, || {
        let current = read_utf8(&project.config_path)?;
        if crate::cli_project::revision(current.as_bytes()) != project.config_digest {
            return Err(coded(
                4,
                "CONFIG_CHANGED",
                "The KnowledgeNestfile changed. Retry the tool operation.",
            ));
        }
        atomic_write(&project.config_path, write_template(&config).as_bytes()).map_err(stage)
    })?;
    let mut output = vec![
        ("name", text(name)),
        ("version", text(tool.version)),
        ("configured", ConfigValue::Bool(!remove)),
    ];
    if !remove {
        output.push(("modules", strings(tool.modules)));
        output.push((
            "configuration",
            if name == "hannoy" {
                hannoy_configuration()
            } else {
                object(vec![
                    ("use", text("loader.auto")),
                    (
                        "with",
                        object(vec![
                            ("unsupported", text("error")),
                            ("ocr", text("loader.ocr")),
                        ]),
                    ),
                ])
            },
        ));
    }
    Ok(object(output))
}

// ---------------------------------------------------------------------------------------------
// User settings.

const OCR_TOOL_NAMES: [&str; 2] = ["oar-ocr", "lightonocr"];

fn providers(role: &str) -> &'static [&'static str] {
    match role {
        "embedding" => &["ollama", "openai"],
        _ => &["ollama", "openai", "strata"],
    }
}

fn invalid(message: impl Into<String>) -> CliError {
    coded(2, "INVALID_ARGUMENT", message)
}

/// `modelName`: a trimmed nonempty name without control characters.
fn model_name(value: &ConfigValue) -> Option<String> {
    let name = value.as_str()?.trim_matches(crate::registry::js_space);
    (!name.is_empty() && !name.chars().any(|c| (c as u32) < 0x20 || c == '\u{7f}'))
        .then(|| name.to_owned())
}

fn global_path(env: &CliEnv) -> String {
    match std::env::var("KNOWLEDGE_NEST_GLOBAL_CONFIG") {
        Ok(path) if !path.is_empty() => env.resolve(&path),
        _ => env.resolve(&node_path::join(
            &env.home,
            &[".knowledge-nest", "config.yaml"],
        )),
    }
}

/// A `{ model, provider }` or `{ tool }` selection of the user settings, checked as zod did.
fn selection(role: &str, value: &ConfigValue) -> Option<ConfigValue> {
    let ConfigValue::Object(entries) = value else {
        return None;
    };
    if role == "ocr" {
        let [(key, tool)] = entries.as_slice() else {
            return None;
        };
        return (key == "tool"
            && tool
                .as_str()
                .is_some_and(|tool| OCR_TOOL_NAMES.contains(&tool)))
        .then(|| value.clone());
    }
    if entries
        .iter()
        .any(|(key, _)| key != "model" && key != "provider")
    {
        return None;
    }
    let model = model_name(value.get("model")?)?;
    let provider = value
        .get("provider")?
        .as_str()
        .filter(|provider| providers(role).contains(provider))?;
    Some(object(vec![
        ("model", text(&model)),
        ("provider", text(provider)),
    ]))
}

/// `readGlobal(path)`: the user settings, or `{ version: 1 }` without a file.
fn read_global(path: &str) -> Result<ConfigValue, CliError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(object(vec![("version", ConfigValue::Number(1.0))]));
        }
        Err(error) => return Err(stage(fs_error("open", path, &error))),
    };
    let fail = || {
        coded(
            2,
            "INVALID_GLOBAL_CONFIG",
            format!(
                "Check {path}. Expected version: 1, optional embedding and llm entries with provider and model, and optional ocr with tool: oar-ocr or lightonocr."
            ),
        )
    };
    let parsed =
        crate::yaml_config::parse_config(&String::from_utf8_lossy(&bytes)).map_err(|_| fail())?;
    let ConfigValue::Object(entries) = &parsed.value else {
        return Err(fail());
    };
    let allowed = ["version", "embedding", "llm", "ocr"];
    if entries
        .iter()
        .any(|(key, _)| !allowed.contains(&key.as_str()))
        || parsed.value.get("version") != Some(&ConfigValue::Number(1.0))
    {
        return Err(fail());
    }
    let mut settings = vec![("version", ConfigValue::Number(1.0))];
    for role in ["embedding", "llm", "ocr"] {
        if let Some(value) = parsed.value.get(role) {
            settings.push((role, selection(role, value).ok_or_else(fail)?));
        }
    }
    Ok(object(settings))
}

fn with_path(path: &str, settings: ConfigValue) -> ConfigValue {
    let mut entries = vec![("path".to_owned(), text(path))];
    if let ConfigValue::Object(fields) = settings {
        entries.extend(fields);
    }
    ConfigValue::Object(entries)
}

/// `configureGlobal(action, role, { provider, model, tool })`
pub fn configure_global(
    env: &CliEnv,
    action: &str,
    role: Option<&str>,
    provider: Option<&str>,
    model: Option<&str>,
    tool: Option<&str>,
) -> Result<ConfigValue, CliError> {
    let path = global_path(env);
    let model_options = provider.is_some() || model.is_some();
    let options = model_options || tool.is_some();
    let ocr = role == Some("ocr");
    if action == "list" && !options {
        return Ok(with_path(&path, read_global(&path)?));
    }
    if (!ocr && !matches!(role, Some("embedding" | "llm"))) || (action == "unset" && options) {
        return Err(invalid(
            "Use config list, config set <embedding|llm> --model <name> [--provider <name>], config set ocr [--tool oar-ocr|lightonocr], or config unset <embedding|llm|ocr>.",
        ));
    }
    let role = role.unwrap_or_default();
    let mut chosen = None;
    if action == "set" {
        if ocr {
            let tool = tool.unwrap_or("oar-ocr");
            if model_options || !OCR_TOOL_NAMES.contains(&tool) {
                return Err(invalid("Use config set ocr [--tool oar-ocr|lightonocr]."));
            }
            chosen = Some(object(vec![("tool", text(tool))]));
        } else {
            let checked = model.and_then(|model| model_name(&text(model)));
            let provider = provider.unwrap_or("ollama");
            if tool.is_some() || checked.is_none() || !providers(role).contains(&provider) {
                return Err(invalid(format!(
                    "Specify a nonempty --model and a --provider from {}.",
                    providers(role).join(", ")
                )));
            }
            chosen = Some(object(vec![
                ("model", text(&checked.unwrap_or_default())),
                ("provider", text(provider)),
            ]));
        }
    }
    let folder = node_path::dirname(&path);
    std::fs::create_dir_all(&folder).map_err(|error| stage(fs_error("mkdir", &folder, &error)))?;
    with_write_lock(&folder, || {
        let ConfigValue::Object(mut settings) = read_global(&path)? else {
            unreachable!()
        };
        if let Some(chosen) = chosen {
            assign(&mut settings, role, Some(chosen));
        } else if settings.iter().any(|(key, _)| key == role) {
            assign(&mut settings, role, None);
        } else {
            return Ok(with_path(&path, ConfigValue::Object(settings)));
        }
        let settings = ConfigValue::Object(settings);
        atomic_write(&path, crate::yaml_emit::stringify(&settings).as_bytes()).map_err(stage)?;
        Ok(with_path(&path, settings))
    })
}

/// `projectModel(kind, { provider, model })`
pub fn project_model(kind: &str, selection: Option<&ConfigValue>) -> ConfigValue {
    let provider = selection
        .and_then(|selection| string_field(selection, "provider"))
        .unwrap_or("ollama");
    let mut entries = vec![("provider", text(provider)), ("kind", text(kind))];
    match provider {
        "ollama" => entries.push(("base_url", text("http://localhost:11434"))),
        "openai" => {
            entries.push(("base_url", text("https://api.openai.com/v1")));
            entries.push(("api_key_env", text("OPENAI_API_KEY")));
        }
        "strata" => entries.push(("base_url", text("http://127.0.0.1:8080/v1"))),
        _ => {}
    }
    entries.push((
        "model",
        selection
            .and_then(|selection| selection.get("model"))
            .filter(|model| !matches!(model, ConfigValue::Null))
            .cloned()
            .unwrap_or(ConfigValue::Null),
    ));
    object(entries)
}

/// The values of `init` that choose models and OCR.
pub struct InitOptions<'a> {
    pub flag: &'a dyn Fn(&str) -> bool,
    pub value: &'a dyn Fn(&str) -> Option<String>,
}

/// The OCR tool and the embedding and chat models a new project starts with.
pub type Selections = (Option<String>, Option<ConfigValue>, Option<ConfigValue>);

/// `initialSettings(type, values)`: `{ ocr, embedding?, llm? }`.
pub fn initial_settings(
    env: &CliEnv,
    kind: &str,
    options: &InitOptions,
) -> Result<Selections, CliError> {
    let current = read_global(&global_path(env))?;
    let tool = (options.value)("ocr-tool");
    if (options.flag)("no-ocr") && ((options.flag)("ocr") || tool.is_some()) {
        return Err(invalid("Use either --ocr or --no-ocr."));
    }
    if let Some(tool) = &tool
        && !OCR_TOOL_NAMES.contains(&tool.as_str())
    {
        return Err(invalid(format!(
            "Use --ocr-tool with {}.",
            OCR_TOOL_NAMES.join(" or ")
        )));
    }
    let ocr = !(options.flag)("no-ocr")
        && ((options.flag)("ocr")
            || tool.is_some()
            || (kind != "mcp" && current.get("ocr").is_some()));
    let ocr = ocr.then(|| {
        tool.clone()
            .or_else(|| {
                current
                    .get("ocr")
                    .and_then(|ocr| string_field(ocr, "tool"))
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "oar-ocr".into())
    });
    let mut selections = (None, None);
    for role in ["embedding", "llm"] {
        let enabled = if role == "embedding" {
            is_search_project(kind)
        } else {
            kind != "mcp"
        };
        let model = (options.value)(&format!("{role}-model"));
        let provider = (options.value)(&format!("{role}-provider"));
        let disabled = (options.flag)(&format!("no-{role}"));
        let specified = model.is_some() || provider.is_some();
        if !enabled {
            if specified || disabled {
                return Err(invalid(format!(
                    "{role} options are unavailable for init {kind}."
                )));
            }
            continue;
        }
        if disabled {
            if specified {
                return Err(invalid(format!(
                    "Use --no-{role} without model or provider flags for the same role."
                )));
            }
            continue;
        }
        if let Some(provider) = &provider
            && !providers(role).contains(&provider.as_str())
        {
            return Err(invalid(format!(
                "Use --{role}-provider with {}.",
                providers(role).join(", ")
            )));
        }
        let checked = match &model {
            None => None,
            Some(model) => Some(
                model_name(&text(model))
                    .ok_or_else(|| invalid(format!("Specify a nonempty --{role}-model.")))?,
            ),
        };
        let fallback = if kind == "mcp" && role == "embedding" && !specified {
            None
        } else {
            current.get(role)
        };
        let fallback_provider = fallback.and_then(|fallback| string_field(fallback, "provider"));
        let selected_provider = provider
            .clone()
            .or_else(|| fallback_provider.map(str::to_owned))
            .unwrap_or_else(|| "ollama".into());
        let selected_model = checked.or_else(|| {
            (Some(selected_provider.as_str()) == fallback_provider)
                .then(|| {
                    fallback
                        .and_then(|fallback| string_field(fallback, "model"))
                        .map(str::to_owned)
                })
                .flatten()
        });
        let selected_model = selected_model.filter(|model| !model.is_empty());
        if selected_model.is_none() && role == "embedding" {
            if provider.is_some() {
                return Err(invalid(
                    "Specify --embedding-model for the selected embedding provider.",
                ));
            }
            continue;
        }
        let mut entries = vec![("provider", text(&selected_provider))];
        if let Some(model) = &selected_model {
            entries.push(("model", text(model)));
        }
        let chosen = object(entries);
        if role == "embedding" {
            selections.0 = Some(chosen);
        } else {
            selections.1 = Some(chosen);
        }
    }
    Ok((ocr, selections.0, selections.1))
}

// ---------------------------------------------------------------------------------------------
// Templates.

/// `defaultEntry(name, overrides)`: `{ use, with }` with every default argument.
fn default_entry(name: &str, overrides: ConfigValue) -> ConfigValue {
    let module = crate::registry::module(name).expect("registered module");
    let mut values = Vec::new();
    for (arg, result) in crate::registry::read_args(module, &overrides) {
        if let crate::registry::ArgResult::Value {
            value: Some(value), ..
        } = result
        {
            values.push((arg.name.to_owned(), value));
        }
    }
    object(vec![
        ("use", text(name)),
        ("with", ConfigValue::Object(values)),
    ])
}

fn empty() -> ConfigValue {
    ConfigValue::Object(Vec::new())
}

/// `ragTemplate(type, path, { embedding, llm })`
pub fn rag_template(
    kind: &str,
    path: &str,
    embedding: Option<&ConfigValue>,
    llm: Option<&ConfigValue>,
) -> ConfigValue {
    let sequences: [(&str, &[&str]); 5] = [
        ("source", &["source.filesystem", "loader.auto"]),
        ("transform", &["text.normalize", "chunk.length"]),
        ("index", &["index.keyword"]),
        (
            "search",
            &["query.normalize", "retrieval.keyword", "context.assemble"],
        ),
        ("ask", &["answer.generate"]),
    ];
    let mut config: Vec<(String, ConfigValue)> = vec![(
        "init".into(),
        object(vec![
            ("version", ConfigValue::Number(1.0)),
            ("type", text(kind)),
        ]),
    )];
    for (stage, names) in sequences {
        if stage == "ask" && kind != "rag" {
            continue;
        }
        let modules = names
            .iter()
            .map(|name| {
                let overrides = match *name {
                    "source.filesystem" => object(vec![("path", text(path))]),
                    "index.keyword" => object(vec![("user_dictionary", ConfigValue::Null)]),
                    _ => empty(),
                };
                default_entry(name, overrides)
            })
            .collect();
        config.push((
            stage.into(),
            object(vec![("modules", ConfigValue::Array(modules))]),
        ));
    }
    let mut models = Vec::new();
    if kind == "rag" {
        models.push(("answerer".to_owned(), project_model("chat", llm)));
    }
    if let Some(embedding) = embedding {
        let reference = || object(vec![("model", text("embedder"))]);
        models.push((
            "embedder".to_owned(),
            project_model("embedding", Some(embedding)),
        ));
        let push = |config: &mut Vec<(String, ConfigValue)>,
                    stage: &str,
                    at: Option<usize>,
                    entries: Vec<ConfigValue>| {
            if let Some((_, ConfigValue::Object(section))) =
                config.iter_mut().find(|(key, _)| key == stage)
                && let Some((_, ConfigValue::Array(modules))) =
                    section.iter_mut().find(|(key, _)| key == "modules")
            {
                let at = at.map_or(modules.len(), |back| modules.len() - back);
                for (offset, entry) in entries.into_iter().enumerate() {
                    modules.insert(at + offset, entry);
                }
            }
        };
        push(
            &mut config,
            "index",
            None,
            vec![default_entry("index.vector", reference())],
        );
        push(
            &mut config,
            "search",
            Some(1),
            vec![
                default_entry("retrieval.vector", reference()),
                default_entry("fusion.rrf", empty()),
            ],
        );
    }
    if !models.is_empty() {
        // `config.models` follows the stages, as the assignments added it.
        config.push(("models".into(), ConfigValue::Object(models)));
    }
    config.push(("serve".into(), crate::config_check::serve_template(kind)));
    ConfigValue::Object(config)
}

/// `wikiTemplate(type, title, purpose, llm)`
pub fn wiki_template(
    kind: &str,
    title: &str,
    purpose: Option<&str>,
    llm: Option<&ConfigValue>,
) -> ConfigValue {
    let entry = |name: &str, args: Vec<(&str, ConfigValue)>| {
        object(vec![("use", text(name)), ("with", object(args))])
    };
    object(vec![
        (
            "init",
            object(vec![
                ("version", ConfigValue::Number(1.0)),
                ("type", text(kind)),
            ]),
        ),
        (
            "about",
            object(vec![
                ("title", text(title)),
                ("purpose", text(purpose.unwrap_or_default())),
                ("out_of_scope", text("")),
            ]),
        ),
        (
            "models",
            object(vec![("writer", project_model("chat", llm))]),
        ),
        (
            "ingress",
            object(vec![(
                "modules",
                ConfigValue::Array(vec![
                    default_entry("loader.auto", empty()),
                    default_entry(
                        "index.keyword",
                        object(vec![
                            ("tokenizer", text("trigram")),
                            ("short_query", text("bigram")),
                        ]),
                    ),
                    entry(
                        if kind == "wiki" {
                            "wiki.write"
                        } else {
                            "para.write"
                        },
                        vec![
                            ("model", text("writer")),
                            ("rules", text("./rules.md")),
                            ("max_pages_per_job", ConfigValue::Number(20.0)),
                        ],
                    ),
                    entry("citation.verify", vec![("on_failure", text("reject"))]),
                ]),
            )]),
        ),
        (
            "query",
            object(vec![(
                "modules",
                ConfigValue::Array(vec![
                    entry(
                        "wiki.search",
                        vec![("include_archived", ConfigValue::Bool(false))],
                    ),
                    entry("wiki.answer", vec![("model", text("writer"))]),
                ]),
            )]),
        ),
        (
            "verify",
            object(vec![
                (
                    "modules",
                    ConfigValue::Array(
                        [
                            "check.citations",
                            "check.stale_versions",
                            "check.links",
                            "check.orphans",
                        ]
                        .iter()
                        .map(|name| entry(name, Vec::new()))
                        .collect(),
                    ),
                ),
                (
                    "content",
                    ConfigValue::Array(vec![entry(
                        "check.contradictions",
                        vec![
                            ("model", text("writer")),
                            ("scope", text("changed_and_related")),
                        ],
                    )]),
                ),
            ]),
        ),
        (
            "archive",
            object(vec![(
                "modules",
                ConfigValue::Array(vec![entry(
                    "wiki.archive",
                    vec![("keep_links", ConfigValue::Bool(true))],
                )]),
            )]),
        ),
        ("serve", crate::config_check::serve_template(kind)),
    ])
}

fn section_comment(section: &str) -> Option<&'static str> {
    Some(match section {
        "source" => "Scan and extract documents. Optional OCR requires a loader.ocr module.",
        "transform" => {
            "Normalize and split text. Additional chunking modules can replace chunk.length."
        }
        "index" => "Build the keyword index. An optional vector index uses index.vector.",
        "search" => "Retrieve candidates and assemble source context.",
        "ask" => "Generate a sourced answer from the search context.",
        "serve" => "Expose the project through its configured transport.",
        _ => return None,
    })
}

/// `writeTemplate(config)`: YAML with a comment on each section and examples for search projects.
pub fn write_template(config: &ConfigValue) -> String {
    let yaml = crate::yaml_emit::stringify(config);
    let kind = config
        .get("init")
        .and_then(|init| string_field(init, "type"))
        .unwrap_or_default();
    let uses = |stage: &str, names: &[&str]| {
        items(config.get(stage).and_then(|section| section.get("modules")))
            .iter()
            .any(|entry| string_field(entry, "use").is_some_and(|name| names.contains(&name)))
    };
    let mut examples: Vec<(&str, &str)> = Vec::new();
    if is_search_project(kind) {
        for (stage, example) in [
            (
                "source",
                "Additional parser: loader.ocr (requires knowledge-nest add ocr).",
            ),
            (
                "transform",
                "Alternative chunking: use: chunk.structure, with: { max_chars: 1600 }.",
            ),
            (
                "index",
                "Additional index: use: index.vector, with: { model: local-embed }.",
            ),
            (
                "search",
                "For hybrid search, add retrieval.vector and fusion.rrf before context.assemble.",
            ),
        ] {
            if (stage == "index" && uses("index", &["index.vector", "hannoy.index"]))
                || (stage == "search" && uses("search", &["fusion.rrf"]))
            {
                continue;
            }
            examples.push((stage, example));
        }
    }
    let mut out = String::with_capacity(yaml.len() + 512);
    for line in yaml.split_inclusive('\n') {
        let bare = line.strip_suffix('\n').unwrap_or(line);
        if let Some(section) = bare.strip_suffix(':') {
            if let Some(index) = examples.iter().position(|(stage, _)| *stage == section) {
                out += &format!("# {}\n", examples.remove(index).1);
            }
            if let Some(comment) = section_comment(section) {
                out += &format!("# {comment}\n");
            }
        }
        out += line;
    }
    out
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
