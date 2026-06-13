use std::path::PathBuf;

fn main() {
    // Prefer a system protoc, but fall back to a vendored binary so the build
    // works on machines that don't have the protobuf compiler installed.
    if std::env::var_os("PROTOC").is_none() {
        if let Ok(path) = protoc_bin_vendored::protoc_bin_path() {
            std::env::set_var("PROTOC", path);
        }
    }

    let proto_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../proto");
    let proto_file = proto_dir.join("hermione.proto");

    println!("cargo:rerun-if-changed={}", proto_file.display());

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&[proto_file], &[proto_dir])
        .expect("failed to compile hermione.proto");
}
