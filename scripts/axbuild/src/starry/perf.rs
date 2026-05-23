use std::{
    env, fs,
    fs::File,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, bail};
use ostool::run::qemu::QemuConfig;
use serde::{Deserialize, Serialize};

use super::{ArgsBuild, ArgsPerf, PerfFormat, Starry, build, rootfs};
use crate::{
    context::{SnapshotPersistence, StarryCliArgs, starry_target_for_arch_checked},
    support::process::ProcessExt,
};

const QPERF_QUEUE_SIZE: usize = 4096;

#[derive(Deserialize, Serialize)]
struct PerfQemuConfig {
    args: Vec<String>,
    uefi: bool,
    to_bin: bool,
    success_regex: Vec<String>,
    fail_regex: Vec<String>,
    shell_prefix: Option<String>,
    shell_init_cmd: Option<String>,
    timeout: Option<u64>,
}

struct QperfTools {
    plugin: PathBuf,
    analyzer: PathBuf,
}

struct PerfOutputs {
    dir: PathBuf,
    raw: PathBuf,
    folded: PathBuf,
    flamegraph: PathBuf,
    summary: PathBuf,
    qemu_config: PathBuf,
}

pub(super) async fn run(starry: &mut Starry, args: ArgsPerf) -> anyhow::Result<()> {
    validate_args(&args)?;
    let arch = args
        .arch
        .clone()
        .unwrap_or_else(|| crate::context::DEFAULT_STARRY_ARCH.to_string());
    let target = starry_target_for_arch_checked(&arch)?.to_string();
    let outputs = prepare_outputs(starry.app.workspace_root(), &arch, args.out.as_deref())?;

    let tools = build_qperf_tools(starry.app.workspace_root())?;

    let build_args = ArgsBuild {
        config: None,
        arch: Some(arch.clone()),
        target: None,
        smp: None,
        debug: true,
    };
    let request = starry.prepare_request(
        StarryCliArgs::from(&build_args),
        None,
        None,
        SnapshotPersistence::Store,
    )?;

    let cargo = build::load_cargo_config(&request)?;
    starry.app.set_debug_mode(true)?;
    starry
        .app
        .build(cargo, request.build_info_path.clone())
        .await?;
    rootfs::ensure_qemu_rootfs_ready(&request, starry.app.workspace_root(), None).await?;
    let cargo = build::load_cargo_config(&request)?;
    let qemu = rootfs::load_patched_qemu_config(starry, &request, &cargo, None, true).await?;

    let perf_qemu = build_perf_qemu_config(&outputs, &tools, &args, qemu.args.clone());
    write_qemu_config_file(&outputs, &perf_qemu)?;

    // Disable debug mode so ostool doesn't add -s -S which would freeze the CPU
    starry.app.set_debug_mode(false)?;
    let result = starry.app.run_qemu(&cargo, perf_qemu).await;
    if let Err(err) = &result {
        if !file_nonempty(&outputs.raw) {
            bail!("qperf QEMU run failed before producing samples: {err}");
        }
        eprintln!("qperf: QEMU ended with error after producing samples: {err}");
    }

    let elf = kernel_elf_path(starry.app.workspace_root(), &target);
    run_analyzer(&tools.analyzer, &elf, &outputs.raw, &outputs.folded)?;

    let flamegraph_generated = if matches!(args.format, PerfFormat::Svg | PerfFormat::All) {
        try_generate_flamegraph(&outputs.folded, &outputs.flamegraph)?
    } else {
        false
    };

    write_summary(
        &outputs,
        &tools,
        &elf,
        &arch,
        &target,
        &args,
        flamegraph_generated,
    )?;
    print_report(&outputs, flamegraph_generated);
    result
}

fn validate_args(args: &ArgsPerf) -> anyhow::Result<()> {
    if args.freq == 0 {
        bail!("--freq must be greater than 0");
    }
    if args.max_depth == 0 {
        bail!("--max-depth must be greater than 0");
    }
    if matches!(args.format, PerfFormat::Pprof) {
        bail!("--format pprof is not supported yet; use --format folded, svg, or all");
    }
    if args.shell_init_cmd.is_some() && args.shell_prefix.is_none() {
        bail!("--shell-init-cmd requires --shell-prefix to detect the shell prompt");
    }
    Ok(())
}

fn build_perf_qemu_config(
    outputs: &PerfOutputs,
    tools: &QperfTools,
    args: &ArgsPerf,
    qemu_args: Vec<String>,
) -> QemuConfig {
    let mut perf_qemu_args = vec!["-plugin".to_string()];
    perf_qemu_args.push(format!(
        "{},freq={},max_depth={},queue_size={},out={}",
        tools.plugin.display(),
        args.freq,
        args.max_depth,
        QPERF_QUEUE_SIZE,
        outputs.raw.display()
    ));
    perf_qemu_args.extend(qemu_args);

    let success_regex = if args.shell_init_cmd.is_some() {
        vec!["BENCH_PASS".to_string()]
    } else {
        Vec::new()
    };

    QemuConfig {
        args: perf_qemu_args,
        uefi: false,
        to_bin: true,
        success_regex,
        fail_regex: vec![r"(?i)\bpanic(?:ked)?\b".to_string()],
        shell_prefix: args.shell_prefix.clone(),
        shell_init_cmd: args.shell_init_cmd.clone(),
        timeout: (args.timeout > 0).then_some(args.timeout),
    }
}

fn prepare_outputs(root: &Path, arch: &str, out: Option<&Path>) -> anyhow::Result<PerfOutputs> {
    let dir = out.map(PathBuf::from).unwrap_or_else(|| {
        root.join("target")
            .join("qperf")
            .join(arch)
            .join(chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string())
    });
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create qperf output directory {}", dir.display()))?;
    Ok(PerfOutputs {
        raw: dir.join("qperf.bin"),
        folded: dir.join("stack.folded"),
        flamegraph: dir.join("flamegraph.svg"),
        summary: dir.join("summary.txt"),
        qemu_config: dir.join("qemu.toml"),
        dir,
    })
}

fn build_qperf_tools(root: &Path) -> anyhow::Result<QperfTools> {
    let manifest = root.join("tools/qperf/Cargo.toml");
    if !manifest.exists() {
        bail!(
            "qperf sources not found at {}; expected tools/qperf to be present",
            manifest.display()
        );
    }

    Command::new("cargo")
        .current_dir(root)
        .args(["build", "--manifest-path"])
        .arg(&manifest)
        .arg("--release")
        .exec()
        .context("failed to build qperf plugin")?;

    Command::new("cargo")
        .current_dir(root)
        .args(["build", "--manifest-path"])
        .arg(&manifest)
        .args(["--release", "-p", "qperf-analyzer"])
        .exec()
        .context("failed to build qperf-analyzer")?;

    let release_dir = root.join("tools/qperf/target/release");
    let tools = QperfTools {
        plugin: release_dir.join("libqperf.so"),
        analyzer: release_dir.join("qperf-analyzer"),
    };
    ensure_file(&tools.plugin, "qperf plugin")?;
    ensure_file(&tools.analyzer, "qperf analyzer")?;
    Ok(tools)
}

fn write_qemu_config_file(
    outputs: &PerfOutputs,
    config: &QemuConfig,
) -> anyhow::Result<()> {
    let perf_config = PerfQemuConfig {
        args: config.args.clone(),
        uefi: config.uefi,
        to_bin: config.to_bin,
        success_regex: config.success_regex.clone(),
        fail_regex: config.fail_regex.clone(),
        shell_prefix: config.shell_prefix.clone(),
        shell_init_cmd: config.shell_init_cmd.clone(),
        timeout: config.timeout,
    };
    fs::write(&outputs.qemu_config, toml::to_string_pretty(&perf_config)?)
        .with_context(|| format!("failed to write {}", outputs.qemu_config.display()))?;
    Ok(())
}

fn run_analyzer(analyzer: &Path, elf: &Path, raw: &Path, folded: &Path) -> anyhow::Result<()> {
    ensure_file(elf, "StarryOS kernel ELF")?;
    ensure_file(raw, "qperf raw samples")?;
    Command::new(analyzer)
        .arg("-e")
        .arg(elf)
        .arg(raw)
        .arg(folded)
        .exec()
        .context("failed to run qperf-analyzer")?;
    ensure_file(folded, "folded stack output")?;
    Ok(())
}

fn try_generate_flamegraph(folded: &Path, svg: &Path) -> anyhow::Result<bool> {
    let Some(generator) = find_flamegraph_generator() else {
        eprintln!(
            "qperf: flamegraph generator not found; install inferno-flamegraph or flamegraph.pl \
             to generate {}",
            svg.display()
        );
        return Ok(false);
    };

    let input =
        File::open(folded).with_context(|| format!("failed to open {}", folded.display()))?;
    let output =
        File::create(svg).with_context(|| format!("failed to create {}", svg.display()))?;
    let status = Command::new(&generator)
        .stdin(Stdio::from(input))
        .stdout(Stdio::from(output))
        .status()
        .with_context(|| format!("failed to run {}", generator.display()))?;
    if !status.success() {
        eprintln!(
            "qperf: flamegraph generator {} failed with {status}; folded stacks are still at {}",
            generator.display(),
            folded.display()
        );
        return Ok(false);
    }
    Ok(true)
}

fn find_flamegraph_generator() -> Option<PathBuf> {
    ["inferno-flamegraph", "flamegraph", "flamegraph.pl"]
        .into_iter()
        .find_map(find_executable)
}

fn find_executable(name: &str) -> Option<PathBuf> {
    let path = Path::new(name);
    if path.components().count() > 1 && path.is_file() {
        return Some(path.to_path_buf());
    }
    env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    })
}

fn write_summary(
    outputs: &PerfOutputs,
    tools: &QperfTools,
    elf: &Path,
    arch: &str,
    target: &str,
    args: &ArgsPerf,
    flamegraph_generated: bool,
) -> anyhow::Result<()> {
    let folded_lines = count_lines(&outputs.folded).unwrap_or(0);
    let plugin_summary = outputs.raw.with_extension("summary.txt");
    let plugin_summary_text = fs::read_to_string(plugin_summary).ok();

    let mut file = File::create(&outputs.summary)
        .with_context(|| format!("failed to create {}", outputs.summary.display()))?;
    writeln!(file, "qperf_format_version = 1")?;
    writeln!(file, "arch = {arch}")?;
    writeln!(file, "target = {target}")?;
    writeln!(file, "frequency_hz = {}", args.freq)?;
    writeln!(file, "max_stack_depth = {}", args.max_depth)?;
    writeln!(file, "queue_size = {QPERF_QUEUE_SIZE}")?;
    writeln!(file, "timeout_seconds = {}", args.timeout)?;
    if args.shell_init_cmd.is_some() {
        writeln!(file, "shell_init_cmd = {}", args.shell_init_cmd.as_deref().unwrap())?;
    }
    writeln!(file, "kernel_elf = {}", elf.display())?;
    writeln!(file, "plugin = {}", tools.plugin.display())?;
    writeln!(file, "analyzer = {}", tools.analyzer.display())?;
    writeln!(file, "raw_samples = {}", outputs.raw.display())?;
    writeln!(file, "folded_stack = {}", outputs.folded.display())?;
    writeln!(file, "folded_stack_lines = {folded_lines}")?;
    writeln!(file, "flamegraph_generated = {flamegraph_generated}")?;
    if flamegraph_generated {
        writeln!(file, "flamegraph = {}", outputs.flamegraph.display())?;
    }
    if let Some(plugin_summary_text) = plugin_summary_text {
        writeln!(file)?;
        writeln!(file, "[plugin_summary]")?;
        write!(file, "{plugin_summary_text}")?;
    } else {
        writeln!(file, "dropped_samples = unknown")?;
        writeln!(
            file,
            "plugin_summary = unavailable; QEMU may have been stopped by timeout before plugin \
             shutdown"
        )?;
    }
    Ok(())
}

fn print_report(outputs: &PerfOutputs, flamegraph_generated: bool) {
    println!("qperf report generated:");
    println!("  output dir: {}", outputs.dir.display());
    println!("  raw samples: {}", outputs.raw.display());
    println!("  folded stack: {}", outputs.folded.display());
    if flamegraph_generated {
        println!("  flamegraph: {}", outputs.flamegraph.display());
    }
    println!("  summary: {}", outputs.summary.display());
}

fn kernel_elf_path(root: &Path, target: &str) -> PathBuf {
    root.join("target")
        .join(target)
        .join("debug")
        .join("starryos")
}

fn ensure_file(path: &Path, label: &str) -> anyhow::Result<()> {
    if file_nonempty(path) {
        Ok(())
    } else {
        bail!("{label} not found or empty at {}", path.display())
    }
}

fn file_nonempty(path: &Path) -> bool {
    path.metadata().map(|meta| meta.len() > 0).unwrap_or(false)
}

fn count_lines(path: &Path) -> anyhow::Result<u64> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut count = 0;
    for line in BufReader::new(file).lines() {
        line?;
        count += 1;
    }
    Ok(count)
}
