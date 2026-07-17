use alloc::string::String;

pub struct CliArgs {
    pub conf_path: String,
    pub libc: String,
    pub ltproot: String,
    pub tmpdir: String,
    #[allow(dead_code)]
    pub no_group_marker: bool,
    pub group_timeout_secs: u64,
}

pub fn parse_cli(argv: &[&str]) -> CliArgs {
    let mut conf_path = String::from("/os_test.conf");
    let mut libc = String::new();
    let mut ltproot = String::new();
    let mut tmpdir = String::from("/tmp");
    let mut no_group_marker = false;
    let mut group_timeout_secs: u64 = 1750;

    let mut i: usize = 1;
    while i < argv.len() {
        match argv[i] {
            "--conf" => {
                i += 1;
                if i < argv.len() {
                    conf_path = String::from(argv[i]);
                }
            }
            "--libc" => {
                i += 1;
                if i < argv.len() {
                    libc = String::from(argv[i]);
                }
            }
            "--ltproot" => {
                i += 1;
                if i < argv.len() {
                    ltproot = String::from(argv[i]);
                }
            }
            "--tmpdir" => {
                i += 1;
                if i < argv.len() {
                    tmpdir = String::from(argv[i]);
                }
            }
            "--no-group-marker" => {
                no_group_marker = true;
            }
            "--group-timeout-secs" => {
                i += 1;
                if i < argv.len() {
                    if let Ok(v) = argv[i].parse::<u64>() {
                        group_timeout_secs = v;
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }

    CliArgs {
        conf_path,
        libc,
        ltproot,
        tmpdir,
        no_group_marker,
        group_timeout_secs,
    }
}
