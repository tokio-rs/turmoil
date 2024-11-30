fn main() -> std::io::Result<()> {
    let fds = protox::compile(["helloworld.proto"], ["."]).unwrap();
    tonic_build::compile_fds(fds)
}
