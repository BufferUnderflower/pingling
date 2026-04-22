use pingling_core_mock::MockCore;
use pingling_core_process::{ProcessCore, ProcessCoreSpec};
use pingling_core_singbox::SingboxCore;
use pingling_domain::VpnCore;

fn main() {
    if let Err(error) = run(std::env::args().skip(1).collect()) {
        eprintln!("{error}");
        std::process::exit(2);
    }
}

fn run(args: Vec<String>) -> Result<(), String> {
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print_help();
        return Ok(());
    }

    let opts = Options::parse(args)?;
    let mut core = opts.build_core()?;

    match opts.command.as_str() {
        "status" => {
            let info = core.info();
            println!(
                "core={} version={} status={}",
                info.name,
                info.version,
                core.status()
            );
            Ok(())
        }
        "check" => {
            for check in core.check_prerequisites() {
                println!(
                    "{}\t{}\t{}",
                    check.name,
                    if check.passed { "ok" } else { "fail" },
                    check.message
                );
            }
            Ok(())
        }
        "smoke" => {
            let config = opts
                .config
                .as_deref()
                .ok_or_else(|| "smoke requires --config <path>".to_owned())?;
            core.validate_config(config)
                .map_err(|error| error.to_string())?;
            core.start(config).map_err(|error| error.to_string())?;
            println!("started: {}", core.status());
            let _ = core.stop();
            println!("stopped: {}", core.status());
            Ok(())
        }
        other => Err(format!("unknown command `{other}`")),
    }
}

#[derive(Debug)]
struct Options {
    core: String,
    binary: Option<String>,
    config: Option<String>,
    command: String,
    start_args: Vec<String>,
    validate_args: Vec<String>,
}

impl Options {
    fn parse(args: Vec<String>) -> Result<Self, String> {
        let mut opts = Self {
            core: "mock".to_owned(),
            binary: None,
            config: None,
            command: "status".to_owned(),
            start_args: Vec::new(),
            validate_args: Vec::new(),
        };

        let mut iter = args.into_iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--core" => opts.core = next_value(&mut iter, "--core")?,
                "--binary" => opts.binary = Some(next_value(&mut iter, "--binary")?),
                "--config" => opts.config = Some(next_value(&mut iter, "--config")?),
                "--start-arg" => opts.start_args.push(next_value(&mut iter, "--start-arg")?),
                "--validate-arg" => opts
                    .validate_args
                    .push(next_value(&mut iter, "--validate-arg")?),
                "status" | "check" | "smoke" => opts.command = arg,
                _ => return Err(format!("unknown argument `{arg}`")),
            }
        }
        Ok(opts)
    }

    fn build_core(&self) -> Result<Box<dyn VpnCore>, String> {
        match self.core.as_str() {
            "mock" => Ok(Box::new(MockCore::new())),
            "singbox" | "sing-box" => Ok(Box::new(SingboxCore::new(
                self.binary.as_deref().unwrap_or("sing-box"),
            ))),
            "process" => {
                let binary = self
                    .binary
                    .as_deref()
                    .ok_or_else(|| "process core requires --binary <path-or-name>".to_owned())?;
                let mut spec = ProcessCoreSpec::new("process", binary);
                if !self.start_args.is_empty() {
                    spec = spec.with_start_args(self.start_args.clone());
                }
                if !self.validate_args.is_empty() {
                    spec = spec.with_validate_args(self.validate_args.clone());
                }
                Ok(Box::new(ProcessCore::new(spec)))
            }
            other => Err(format!("unsupported core `{other}`")),
        }
    }
}

fn next_value(iter: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    iter.next()
        .filter(|value| !value.starts_with("--"))
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn print_help() {
    println!(
        "pingling-primitive-host\n\
         \n\
         Usage:\n\
           pingling-primitive-host [--core mock|process|singbox] <status|check|smoke>\n\
           pingling-primitive-host --core process --binary /bin/echo --config ./config.json smoke\n\
         \n\
         Options:\n\
           --binary <path-or-name>       process or sing-box binary\n\
           --config <path>              config path for smoke\n\
           --start-arg <arg>            process start arg, may repeat, supports {{config}}\n\
           --validate-arg <arg>         process validate arg, may repeat, supports {{config}}\n"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_mock_status() {
        let opts = Options::parse(vec![]).unwrap();

        assert_eq!(opts.core, "mock");
        assert_eq!(opts.command, "status");
        assert!(opts.binary.is_none());
    }

    #[test]
    fn parses_process_core_arguments() {
        let opts = Options::parse(vec![
            "--core".into(),
            "process".into(),
            "--binary".into(),
            "/bin/echo".into(),
            "--config".into(),
            "config.json".into(),
            "--start-arg".into(),
            "run".into(),
            "--start-arg".into(),
            "{config}".into(),
            "--validate-arg".into(),
            "check".into(),
            "smoke".into(),
        ])
        .unwrap();

        assert_eq!(opts.core, "process");
        assert_eq!(opts.binary.as_deref(), Some("/bin/echo"));
        assert_eq!(opts.config.as_deref(), Some("config.json"));
        assert_eq!(opts.start_args, vec!["run", "{config}"]);
        assert_eq!(opts.validate_args, vec!["check"]);
        assert_eq!(opts.command, "smoke");
    }

    #[test]
    fn process_core_requires_binary() {
        let opts = Options::parse(vec!["--core".into(), "process".into()]).unwrap();

        let error = match opts.build_core() {
            Ok(_) => panic!("process core without binary should fail"),
            Err(error) => error,
        };

        assert!(error.contains("--binary"));
    }

    #[test]
    fn mock_core_can_be_constructed() {
        let opts = Options::parse(vec!["--core".into(), "mock".into()]).unwrap();
        let core = opts.build_core().unwrap();

        assert_eq!(core.info().name, "mock");
    }
}
