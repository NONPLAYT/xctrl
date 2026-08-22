fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protos = [
        "proto/app/stats/command/command.proto",
        "proto/app/proxyman/command/command.proto",
        "proto/proxy/vless/account.proto",
    ];
    tonic_prost_build::configure()
        .build_server(false)
        .include_file("xray.rs")
        .compile_protos(&protos, &["proto"])?;
    println!("cargo:rerun-if-changed=proto");
    Ok(())
}
