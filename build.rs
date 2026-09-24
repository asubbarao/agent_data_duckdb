use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;

const WINDOWS_GNU_TARGET: &str = "x86_64-pc-windows-gnu";

#[derive(Clone, Copy)]
enum CandidateKind {
    Compiler,
    LinkerProbe,
}

#[derive(Debug)]
struct CompilerInfo {
    target: String,
    libgcc: String,
}

fn compiler_candidates<F>(target: &str, get_env: F) -> Vec<(String, CandidateKind)>
where
    F: Fn(&str) -> Option<String>,
{
    let target_underscored = target.replace('-', "_");
    let cargo_target = target_underscored.to_ascii_uppercase();
    let target_compiler_keys = [
        format!("CC_{target}"),
        format!("CC_{target_underscored}"),
        "TARGET_CC".to_string(),
    ];
    let mut candidates = target_compiler_keys
        .into_iter()
        .filter_map(|key| get_env(&key).filter(|value| !value.trim().is_empty()))
        .map(|value| (value, CandidateKind::Compiler))
        .collect::<Vec<_>>();

    if let Some(linker) = get_env(&format!("CARGO_TARGET_{cargo_target}_LINKER"))
        .filter(|value| !value.trim().is_empty())
    {
        candidates.push((linker, CandidateKind::LinkerProbe));
    }
    if let Some(compiler) = get_env("CC").filter(|value| !value.trim().is_empty()) {
        candidates.push((compiler, CandidateKind::Compiler));
    }
    candidates.push(("x86_64-w64-mingw32-gcc".to_string(), CandidateKind::Compiler));
    candidates
}

fn is_x86_64_mingw(compiler_target: &str) -> bool {
    let target = compiler_target.trim().to_ascii_lowercase();
    target.starts_with("x86_64") && target.contains("mingw")
}

fn command_output(compiler: &str, argument: &str) -> Result<String, String> {
    let output = Command::new(compiler)
        .arg(argument)
        .output()
        .map_err(|error| format!("cannot run '{compiler} {argument}': {error}"))?;
    if !output.status.success() {
        return Err(format!("'{compiler} {argument}' exited with {}", output.status));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn compiler_info(compiler: &str) -> Result<CompilerInfo, String> {
    Ok(CompilerInfo {
        target: command_output(compiler, "-dumpmachine")?,
        libgcc: command_output(compiler, "-print-libgcc-file-name")?,
    })
}

fn select_target_compiler<F, G>(target: &str, get_env: F, inspect: G) -> Result<(String, CompilerInfo), String>
where
    F: Fn(&str) -> Option<String>,
    G: Fn(&str) -> Result<CompilerInfo, String>,
{
    for (compiler, kind) in compiler_candidates(target, get_env) {
        match inspect(&compiler) {
            Ok(info) if is_x86_64_mingw(&info.target) => return Ok((compiler, info)),
            Ok(info) => {
                return Err(format!(
                    "MinGW compiler '{compiler}' targets '{}', expected x86_64 MinGW; refusing its libgcc archive",
                    info.target
                ));
            }
            Err(_) if matches!(kind, CandidateKind::LinkerProbe) => continue,
            Err(error) => return Err(error),
        }
    }
    Err("no configured MinGW compiler supplied libgcc".to_string())
}

fn install_linker_alias(libgcc: &str, out_dir: &str) -> Result<(), String> {
    if !Path::new(libgcc).is_file() {
        return Err(format!("MinGW compiler reported missing libgcc archive: {libgcc}"));
    }
    let alias = Path::new(out_dir).join("libgcc_eh.a");
    // Rust's GNU target link specification names -lgcc_eh; Rtools exposes this
    // compiler runtime archive as libgcc.a, so provide the linker-name alias.
    fs::copy(libgcc, &alias).map_err(|error| {
        format!("cannot provide MinGW libgcc_eh linker-name alias from '{libgcc}': {error}")
    })?;
    Ok(())
}

fn main() {
    let target_underscored = WINDOWS_GNU_TARGET.replace('-', "_");
    for key in [
        format!("CC_{WINDOWS_GNU_TARGET}"),
        format!("CC_{target_underscored}"),
        "TARGET_CC".to_string(),
        "CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER".to_string(),
        "CC".to_string(),
    ] {
        println!("cargo:rerun-if-env-changed={key}");
    }

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("gnu")
        || env::var("TARGET").as_deref() != Ok(WINDOWS_GNU_TARGET)
    {
        return;
    }

    let (_, info) = select_target_compiler(WINDOWS_GNU_TARGET, |key| env::var(key).ok(), compiler_info)
        .unwrap_or_else(|error| panic!("{error}"));
    let out_dir = env::var("OUT_DIR").expect("Cargo supplies OUT_DIR");
    install_linker_alias(&info.libgcc, &out_dir).unwrap_or_else(|error| panic!("{error}"));
    println!("cargo:rustc-link-search=native={out_dir}");
}

#[cfg(test)]
mod tests {
    use super::{compiler_info, install_linker_alias, select_target_compiler, CompilerInfo, WINDOWS_GNU_TARGET};
    use std::collections::HashMap;

    fn info(target: &str) -> CompilerInfo {
        CompilerInfo {
            target: target.to_string(),
            libgcc: "/fixtures/libgcc.a".to_string(),
        }
    }

    #[test]
    fn target_compiler_precedes_generic_host_compiler() {
        let values = HashMap::from([
            ("CC_x86_64_pc_windows_gnu".to_string(), "/fixtures/target-gcc".to_string()),
            ("CC".to_string(), "/fixtures/host-gcc".to_string()),
        ]);
        let selected = select_target_compiler(
            WINDOWS_GNU_TARGET,
            |key| values.get(key).cloned(),
            |compiler| match compiler {
                "/fixtures/target-gcc" => Ok(info("x86_64-w64-mingw32")),
                "/fixtures/host-gcc" => Ok(info("x86_64-unknown-linux-gnu")),
                other => Err(format!("unexpected compiler {other}")),
            },
        )
        .unwrap();

        assert_eq!(selected.0, "/fixtures/target-gcc");
    }

    #[test]
    fn non_compiler_cargo_linker_falls_through_to_compiler() {
        let values = HashMap::from([
            (
                "CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER".to_string(),
                "/fixtures/target-ld".to_string(),
            ),
            ("CC".to_string(), "/fixtures/target-gcc".to_string()),
        ]);
        let selected = select_target_compiler(
            WINDOWS_GNU_TARGET,
            |key| values.get(key).cloned(),
            |compiler| match compiler {
                "/fixtures/target-ld" => Err("does not support GCC queries".to_string()),
                "/fixtures/target-gcc" => Ok(info("x86_64-w64-mingw32")),
                other => Err(format!("unexpected compiler {other}")),
            },
        )
        .unwrap();

        assert_eq!(selected.0, "/fixtures/target-gcc");
    }

    #[test]
    fn rejects_host_compiler_archives() {
        let values = HashMap::from([("CC".to_string(), "/fixtures/host-gcc".to_string())]);
        let error = select_target_compiler(
            WINDOWS_GNU_TARGET,
            |key| values.get(key).cloned(),
            |_| Ok(info("x86_64-unknown-linux-gnu")),
        )
        .unwrap_err();

        assert!(error.contains("refusing its libgcc archive"));
    }

    #[cfg(unix)]
    #[test]
    fn compiler_probe_and_alias_copy_use_real_commands() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!("agent-data-build-script-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let archive = root.join("libgcc.a");
        let compiler = root.join("target-gcc");
        let out_dir = root.join("out");
        std::fs::write(&archive, b"target runtime archive").unwrap();
        std::fs::create_dir_all(&out_dir).unwrap();
        std::fs::write(
            &compiler,
            format!(
                "#!/bin/sh\ncase \"$1\" in\n-dumpmachine) echo x86_64-w64-mingw32 ;;\n-print-libgcc-file-name) echo '{}' ;;\nesac\n",
                archive.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&compiler, std::fs::Permissions::from_mode(0o755)).unwrap();

        let probed = compiler_info(compiler.to_str().unwrap()).unwrap();
        assert_eq!(probed.target, "x86_64-w64-mingw32");
        install_linker_alias(&probed.libgcc, out_dir.to_str().unwrap()).unwrap();
        assert_eq!(std::fs::read(out_dir.join("libgcc_eh.a")).unwrap(), b"target runtime archive");
        std::fs::remove_dir_all(root).unwrap();
    }
}
