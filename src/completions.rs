use std::collections::HashMap;
use std::process::Command;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlagCompletion {
    pub flag: String,
    pub description: String,
}

#[derive(Clone, Default)]
pub struct HelpCompletionExtractor {
    cache: Arc<Mutex<HashMap<String, Vec<FlagCompletion>>>>,
}

impl HelpCompletionExtractor {
    pub fn new() -> Self {
        Self {
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn complete_flags(&self, command: &str, prefix: &str) -> Vec<FlagCompletion> {
        let completions = self.get_or_extract(command);
        completions
            .into_iter()
            .filter(|c| c.flag.starts_with(prefix))
            .collect()
    }

    pub fn get_or_extract(&self, command: &str) -> Vec<FlagCompletion> {
        if let Ok(guard) = self.cache.lock() {
            if let Some(cached) = guard.get(command) {
                return cached.clone();
            }
        }

        let extracted = Self::extract(command);
        if let Ok(mut guard) = self.cache.lock() {
            guard.insert(command.to_string(), extracted.clone());
        }
        extracted
    }

    fn extract(command: &str) -> Vec<FlagCompletion> {
        let output = Command::new(command)
            .arg("--help")
            .output()
            .ok()
            .filter(|o| o.status.success() || !o.stdout.is_empty())
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .or_else(|| {
                Command::new(command)
                    .arg("-h")
                    .output()
                    .ok()
                    .filter(|o| o.status.success() || !o.stdout.is_empty())
                    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            });

        let Some(help_text) = output else {
            return Vec::new();
        };

        Self::parse_help_text(&help_text)
    }

    pub fn parse_help_text(help_text: &str) -> Vec<FlagCompletion> {
        let mut results = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for line in help_text.lines() {
            let trimmed = line.trim();
            if !trimmed.starts_with('-') {
                continue;
            }

            let (flags_part, desc_part) = match trimmed.find("  ") {
                Some(idx) => (&trimmed[..idx], trimmed[idx..].trim()),
                None => (trimmed, ""),
            };

            for token in flags_part.split(',') {
                let token = token.trim();
                let flag = if let Some(eq_idx) = token.find('=') {
                    &token[..eq_idx]
                } else if let Some(space_idx) = token.find(' ') {
                    &token[..space_idx]
                } else {
                    token
                };

                if (flag.starts_with("--") && flag.len() > 2) || (flag.starts_with('-') && flag.len() == 2) {
                    if seen.insert(flag.to_string()) {
                        results.push(FlagCompletion {
                            flag: flag.to_string(),
                            description: desc_part.to_string(),
                        });
                    }
                }
            }
        }

        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gnu_and_clap_help_flags() {
        let sample_help = "
Usage: mytool [OPTIONS]

Options:
  -f, --force                  Force overwrite of existing files
  -v, --verbose                Enable verbose debug output
      --format <FMT>           Output format [default: text]
      --config=PATH            Path to custom configuration file
  -h, --help                   Print help information
";
        let completions = HelpCompletionExtractor::parse_help_text(sample_help);
        let flags: Vec<&str> = completions.iter().map(|c| c.flag.as_str()).collect();

        assert!(flags.contains(&"-f"));
        assert!(flags.contains(&"--force"));
        assert!(flags.contains(&"-v"));
        assert!(flags.contains(&"--verbose"));
        assert!(flags.contains(&"--format"));
        assert!(flags.contains(&"--config"));
        assert!(flags.contains(&"-h"));
        assert!(flags.contains(&"--help"));

        let force_comp = completions.iter().find(|c| c.flag == "--force").unwrap();
        assert_eq!(force_comp.description, "Force overwrite of existing files");
    }

    #[test]
    fn filters_completions_by_prefix() {
        let extractor = HelpCompletionExtractor::new();
        let sample_help = "
  -a, --all        Show all items
  -b, --batch      Run in batch mode
      --atomic     Perform atomic commit
";
        let parsed = HelpCompletionExtractor::parse_help_text(sample_help);
        extractor.cache.lock().unwrap().insert("dummy".into(), parsed);

        let completions = extractor.complete_flags("dummy", "--a");
        let flags: Vec<&str> = completions.iter().map(|c| c.flag.as_str()).collect();
        assert_eq!(flags, vec!["--all", "--atomic"]);
    }
}
