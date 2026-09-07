use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=resources");
    let target = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"))
        .join("lg-buddy-gui.gresource");
    let status = Command::new("glib-compile-resources")
        .args(["--sourcedir=resources", "--target"])
        .arg(target)
        .arg("resources/resources.gresource.xml")
        .status()
        .expect("glib-compile-resources is required to build the GTK frontend");
    assert!(status.success(), "could not compile GUI resources");
}
