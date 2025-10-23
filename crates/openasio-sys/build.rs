fn main(){ println!("cargo:rerun-if-changed=../../sdk/include/openasio/openasio.h"); println!("cargo:include=../../sdk/include"); }
