// SPDX-License-Identifier: GPL-3.0-or-later

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The schema is a startup invariant: fail the build if it does not
    // exist or does not compile.
    prost_build::compile_protos(&["proto/control.proto"], &["proto/"])?;
    println!("cargo:rerun-if-changed=proto/control.proto");
    Ok(())
}
