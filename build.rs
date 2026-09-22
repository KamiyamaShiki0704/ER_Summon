//! Embed a VERSIONINFO resource into the cdylib.
//!
//! A DLL that carries no version resource at all gives an endpoint security
//! agent nothing to attribute the file to, so it falls back to pure
//! heuristic scoring, and a stripped cdylib that rewrites another process'
//! memory scores badly there. Filling in the standard fields is the cheapest
//! signal that this is the ordinary software it actually is.
//!
//! This deliberately does not use a build dependency: the Windows SDK
//! resource compiler is invoked directly, and when it cannot be found the
//! resource is skipped with a warning instead of failing the build.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc")
    {
        return;
    }

    let Some(rc) = find_rc() else {
        println!(
            "cargo:warning=VERSIONINFO not embedded: rc.exe was not found. \
             Install the Windows SDK or point the RC environment variable at it."
        );
        return;
    };

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("cargo always sets OUT_DIR"));
    let rc_path = out_dir.join("summon-version.rc");
    let res_path = out_dir.join("summon-version.res");

    if let Err(error) = fs::write(&rc_path, resource_script()) {
        println!("cargo:warning=VERSIONINFO not embedded: could not write {rc_path:?}: {error}");
        return;
    }

    match Command::new(&rc)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res_path)
        .arg(&rc_path)
        .status()
    {
        Ok(status) if status.success() && res_path.is_file() => {
            // The path is handed to the linker verbatim, so it has to stay
            // free of spaces. OUT_DIR sits under target/ and does not.
            println!("cargo:rustc-cdylib-link-arg={}", res_path.display());
        }
        Ok(status) => {
            println!("cargo:warning=VERSIONINFO not embedded: rc.exe exited with {status}");
        }
        Err(error) => {
            println!("cargo:warning=VERSIONINFO not embedded: could not run {rc:?}: {error}");
        }
    }
}

/// Locate `rc.exe`: an explicit `RC` override, then `PATH`, then the
/// installed Windows SDKs, newest first.
fn find_rc() -> Option<PathBuf> {
    if let Some(explicit) = env::var_os("RC") {
        let candidate = PathBuf::from(explicit);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    if let Ok(output) = Command::new("where.exe").arg("rc.exe").output()
        && output.status.success()
    {
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let candidate = PathBuf::from(line.trim());
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    for key in ["ProgramFiles(x86)", "ProgramFiles"] {
        let Some(root) = env::var_os(key) else {
            continue;
        };
        let Ok(entries) = fs::read_dir(Path::new(&root).join("Windows Kits").join("10").join("bin"))
        else {
            continue;
        };
        let mut versions: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        versions.sort();
        for version in versions.into_iter().rev() {
            for arch in ["x64", "x86"] {
                let candidate = version.join(arch).join("rc.exe");
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }

    None
}

fn resource_script() -> String {
    let version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let mut parts: Vec<String> = version
        .split('.')
        .map(|part| {
            part.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
        })
        .collect();
    while parts.len() < 4 {
        parts.push("0".into());
    }
    parts.truncate(4);

    let comma = parts.join(",");
    let dotted = parts.join(".");

    format!(
        r#"1 VERSIONINFO
 FILEVERSION {comma}
 PRODUCTVERSION {comma}
 FILEFLAGSMASK 0x3fL
 FILEFLAGS 0x0L
 FILEOS 0x40004L
 FILETYPE 0x2L
 FILESUBTYPE 0x0L
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "CompanyName", "ER_Summon"
      VALUE "FileDescription", "Elden Ring Nightreign attack summon mod"
      VALUE "FileVersion", "{dotted}"
      VALUE "InternalName", "summon"
      VALUE "LegalCopyright", "MIT OR Apache-2.0"
      VALUE "OriginalFilename", "summon.dll"
      VALUE "ProductName", "Nightreign Attack Summon"
      VALUE "ProductVersion", "{dotted}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    )
}
