use std::cell::RefCell;
use std::env;
use std::fs;
use std::path::Path;

const LITERAL_START: char = '\u{1}';
const LITERAL_END: char = '\u{2}';
const ESCAPED_START: char = '\u{3}';
const ESCAPED_END: char = '\u{4}';
const DOUBLE_QUOTE_MARKER: char = '\u{5}';

pub type SubshellExecutorFn = fn(&str, i32) -> Result<String, String>;

thread_local! {
    static POSITIONAL_PARAMS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static SUBSHELL_EXECUTOR: RefCell<Option<SubshellExecutorFn>> = const { RefCell::new(None) };
}

pub fn set_positional_params(args: Vec<String>) {
    POSITIONAL_PARAMS.with(|params| *params.borrow_mut() = args);
}

pub fn get_positional_params() -> Vec<String> {
    POSITIONAL_PARAMS.with(|params| params.borrow().clone())
}

pub fn register_subshell_executor(executor: fn(&str, i32) -> Result<String, String>) {
    SUBSHELL_EXECUTOR.with(|hook| *hook.borrow_mut() = Some(executor));
}

pub fn execute_subshell(command: &str, last_status: i32) -> Result<String, String> {
    let hook = SUBSHELL_EXECUTOR.with(|h| *h.borrow());
    if let Some(executor) = hook {
        executor(command, last_status)
    } else {
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(command)
            .output()
            .map_err(|err| err.to_string())?;
        let mut result = String::from_utf8_lossy(&output.stdout).into_owned();
        while result.ends_with('\n') || result.ends_with('\r') {
            result.pop();
        }
        Ok(result)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogicalOp {
    And,
    Or,
}

#[derive(Debug, Clone)]
pub struct SequenceItem {
    pub op: Option<LogicalOp>,
    pub node: Ast,
    pub background: bool,
    pub raw_tokens: Vec<Token>,
}

#[derive(Debug, Clone)]
pub struct AstSequence {
    pub items: Vec<SequenceItem>,
}


#[derive(Debug, Clone)]
pub struct ParsedFor {
    pub variable: String,
    pub values: Vec<String>,
    pub body_tokens: Vec<Token>,
}

impl ParsedFor {
    pub fn parse_body(&self, last_status: i32) -> Result<Vec<Ast>, String> {
        parse_branch(&self.body_tokens, last_status)
    }
}

#[derive(Debug, Clone)]
pub struct ParsedWhile {
    pub condition_tokens: Vec<Token>,
    pub body_tokens: Vec<Token>,
}

impl ParsedWhile {
    pub fn parse_condition(&self, last_status: i32) -> Result<Vec<Ast>, String> {
        parse_branch(&self.condition_tokens, last_status)
    }

    pub fn parse_body(&self, last_status: i32) -> Result<Vec<Ast>, String> {
        parse_branch(&self.body_tokens, last_status)
    }
}

#[derive(Debug, Clone)]
pub struct ParsedFunction {
    pub name: String,
    pub body_tokens: Vec<Token>,
}

impl ParsedFunction {
    pub fn parse_body(&self, last_status: i32) -> Result<Vec<Ast>, String> {
        parse_branch(&self.body_tokens, last_status)
    }
}

#[derive(Debug, Clone)]
pub enum Ast {
    Pipeline(ParsedPipeline),
    If(ParsedIf),
    For(ParsedFor),
    While(ParsedWhile),
    Function(ParsedFunction),
    Sequence(AstSequence),
    StructuredPipeline(ParsedStructuredPipeline),
}

#[derive(Debug, Clone)]
pub struct ParsedStructuredPipeline {
    pub source: Box<Ast>,
    pub stages: Vec<StructuredOp>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StructuredOp {
    Project(String),
    Filter(FilterExpr),
    Take(usize),
    Skip(usize),
    Count,
    Format(OutputFormat),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputFormat {
    Json,
    Toml,
    Yaml,
    Table,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterExpr {
    pub field: String,
    pub op: FilterOp,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterOp {
    Equal,
    NotEqual,
    GreaterThan,
    LessThan,
    GreaterOrEqual,
    LessOrEqual,
    Exists,
}

#[derive(Debug, Clone)]
pub struct ParsedIf {
    pub condition: ParsedPipeline,
    pub then_branch: Vec<Ast>,
    pub else_branch: Option<Vec<Ast>>,
}

#[derive(Debug, Clone)]
pub struct ParsedCommand {
    pub program: String,
    pub args: Vec<String>,
    pub environment: Vec<(String, String)>,
    pub redirects: Vec<Redirection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Redirection {
    Stdin(String),
    Stdout(String),
    StdoutAppend(String),
    Stderr(String),
    StderrAppend(String),
    StdoutAndStderr(String),
    StdoutAndStderrAppend(String),
    DupRead(i32, i32),
    DupWrite(i32, i32),
    HereString(String),
}

#[derive(Debug, Clone)]
pub struct ParsedPipeline {
    pub commands: Vec<ParsedCommand>,
    pub background: bool,
}

#[allow(dead_code)]
pub fn parse_input(input: &str) -> Result<Option<ParsedCommand>, String> {
    match parse_ast(input, 0)? {
        Some(Ast::Pipeline(pipeline)) => {
            if pipeline.commands.len() != 1 {
                return Err("pipelines are not valid in this context".into());
            }
            Ok(pipeline.commands.into_iter().next())
        }
        Some(Ast::If(_)) => Err("if statements are not valid in this context".into()),
        Some(Ast::For(_)) => Err("for loops are not valid in this context".into()),
        Some(Ast::While(_)) => Err("while loops are not valid in this context".into()),
        Some(Ast::Function(_)) => Err("function declarations are not valid in this context".into()),
        Some(Ast::Sequence(_)) => Err("sequences are not valid in this context".into()),
        Some(Ast::StructuredPipeline(_)) => {
            Err("structured pipelines are not valid in this context".into())
        }
        None => Ok(None),
    }
}

pub fn parse_ast(input: &str, last_status: i32) -> Result<Option<Ast>, String> {
    let tokens = tokenize(input)?;
    if tokens.is_empty() {
        return Ok(None);
    }
    parse_ast_from_tokens(&tokens, last_status)
}

pub fn parse_ast_from_tokens(tokens: &[Token], last_status: i32) -> Result<Option<Ast>, String> {
    if tokens.is_empty() {
        return Ok(None);
    }

    let has_sequence_tokens = tokens.iter().any(|t| {
        matches!(
            t,
            Token::Semicolon | Token::Newline | Token::LogicalAnd | Token::LogicalOr
        )
    });

    if !has_sequence_tokens {
        if tokens.contains(&Token::StructuredPipe) {
            return Ok(Some(parse_structured_pipeline(tokens, last_status)?));
        }
        if let Token::If = tokens[0] {
            return Ok(Some(Ast::If(parse_if(tokens, last_status)?)));
        }
        if let Some(func) = try_parse_function(tokens, last_status)? {
            return Ok(Some(Ast::Function(func)));
        }
        if let Token::Word(w) = &tokens[0] {
            if w == "for" {
                return Ok(Some(Ast::For(parse_for(tokens, last_status)?)));
            } else if w == "while" {
                return Ok(Some(Ast::While(parse_while(tokens, last_status)?)));
            }
        }
        return Ok(Some(Ast::Pipeline(parse_pipeline_from_tokens(
            tokens,
            last_status,
        )?)));
    }

    // Split into sequence items
    let mut items = Vec::new();
    let mut current_tokens = Vec::new();
    let mut pending_op = None;
    let mut i = 0;

    while i < tokens.len() {
        match &tokens[i] {
            Token::If => {
                let mut depth = 1;
                current_tokens.push(tokens[i].clone());
                i += 1;
                while i < tokens.len() && depth > 0 {
                    if matches!(tokens[i], Token::If) {
                        depth += 1;
                    } else if matches!(tokens[i], Token::Fi) {
                        depth -= 1;
                    }
                    current_tokens.push(tokens[i].clone());
                    i += 1;
                }
                if depth > 0 {
                    return Err("missing 'fi' for if statement".into());
                }
                continue;
            }
            Token::Word(w) if w == "for" || w == "while" => {
                let mut depth = 1;
                current_tokens.push(tokens[i].clone());
                i += 1;
                while i < tokens.len() && depth > 0 {
                    if let Token::Word(w2) = &tokens[i] {
                        if w2 == "for" || w2 == "while" || w2 == "if" {
                            depth += 1;
                        } else if w2 == "done" || w2 == "fi" {
                            depth -= 1;
                        }
                    } else if matches!(tokens[i], Token::If) {
                        depth += 1;
                    } else if matches!(tokens[i], Token::Fi) {
                        depth -= 1;
                    }
                    current_tokens.push(tokens[i].clone());
                    i += 1;
                }
                if depth > 0 {
                    return Err("missing 'done' or 'fi' for block".into());
                }
                continue;
            }
            Token::Word(w) if w == "{" => {
                let mut depth = 1;
                current_tokens.push(tokens[i].clone());
                i += 1;
                while i < tokens.len() && depth > 0 {
                    if let Token::Word(w2) = &tokens[i] {
                        if w2 == "{" {
                            depth += 1;
                        } else if w2 == "}" {
                            depth -= 1;
                        }
                    }
                    current_tokens.push(tokens[i].clone());
                    i += 1;
                }
                continue;
            }
            Token::LogicalAnd | Token::LogicalOr | Token::Semicolon | Token::Newline => {
                let next_op = match tokens[i] {
                    Token::LogicalAnd => Some(LogicalOp::And),
                    Token::LogicalOr => Some(LogicalOp::Or),
                    _ => None,
                };

                if !current_tokens.is_empty() {
                    let background = current_tokens.last() == Some(&Token::Background);
                    if background {
                        current_tokens.pop();
                    }
                    let raw_tokens = current_tokens.clone();
                    let node = if current_tokens.contains(&Token::StructuredPipe) {
                        parse_structured_pipeline(&current_tokens, last_status)?
                    } else if let Some(func) = try_parse_function(&current_tokens, last_status)? {
                        Ast::Function(func)
                    } else if let Token::If = current_tokens[0] {
                        Ast::If(parse_if(&current_tokens, last_status)?)
                    } else if let Token::Word(w) = &current_tokens[0] {
                        if w == "for" {
                            Ast::For(parse_for(&current_tokens, last_status)?)
                        } else if w == "while" {
                            Ast::While(parse_while(&current_tokens, last_status)?)
                        } else {
                            Ast::Pipeline(parse_pipeline_from_tokens(&current_tokens, last_status)?)
                        }
                    } else {
                        Ast::Pipeline(parse_pipeline_from_tokens(&current_tokens, last_status)?)
                    };
                    items.push(SequenceItem {
                        op: pending_op,
                        node,
                        background,
                        raw_tokens,
                    });
                    current_tokens.clear();
                    pending_op = next_op;
                } else if next_op.is_some() {
                    pending_op = next_op;
                }
            }
            token => {
                current_tokens.push(token.clone());
            }
        }
        i += 1;
    }

    if !current_tokens.is_empty() {
        let raw_tokens = current_tokens.clone();
        let background = current_tokens.last() == Some(&Token::Background);
        if background {
            current_tokens.pop();
        }
        let node = if current_tokens.contains(&Token::StructuredPipe) {
            parse_structured_pipeline(&current_tokens, last_status)?
        } else if let Some(func) = try_parse_function(&current_tokens, last_status)? {
            Ast::Function(func)
        } else if let Token::If = current_tokens[0] {
            Ast::If(parse_if(&current_tokens, last_status)?)
        } else if let Token::Word(w) = &current_tokens[0] {
            if w == "for" {
                Ast::For(parse_for(&current_tokens, last_status)?)
            } else if w == "while" {
                Ast::While(parse_while(&current_tokens, last_status)?)
            } else {
                Ast::Pipeline(parse_pipeline_from_tokens(&current_tokens, last_status)?)
            }
        } else {
            Ast::Pipeline(parse_pipeline_from_tokens(&current_tokens, last_status)?)
        };
        items.push(SequenceItem {
            op: pending_op,
            node,
            background,
            raw_tokens,
        });
    }

    if items.is_empty() {
        return Ok(None);
    }

    if items.len() == 1 && items[0].op.is_none() && !items[0].background {
        return Ok(Some(items.remove(0).node));
    }

    Ok(Some(Ast::Sequence(AstSequence { items })))
}

fn parse_structured_pipeline(tokens: &[Token], last_status: i32) -> Result<Ast, String> {
    let mut parts: Vec<Vec<Token>> = Vec::new();
    let mut current = Vec::new();

    for token in tokens {
        if token == &Token::StructuredPipe {
            parts.push(std::mem::take(&mut current));
        } else {
            current.push(token.clone());
        }
    }
    parts.push(current);

    if parts.len() < 2 {
        return Err("expected structured pipe '|>'".into());
    }

    let source_tokens = &parts[0];
    if source_tokens.is_empty() {
        return Err("missing command before '|>'".into());
    }

    let source = parse_ast_from_tokens(source_tokens, last_status)?
        .ok_or_else(|| "empty source for structured pipe".to_string())?;

    let mut stages = Vec::new();
    for stage_tokens in &parts[1..] {
        if stage_tokens.is_empty() {
            return Err("missing operation after '|>'".into());
        }
        let op = parse_structured_op(stage_tokens, last_status)?;
        stages.push(op);
    }

    Ok(Ast::StructuredPipeline(ParsedStructuredPipeline {
        source: Box::new(source),
        stages,
    }))
}

fn parse_structured_op(tokens: &[Token], last_status: i32) -> Result<StructuredOp, String> {
    let mut words = Vec::new();
    for token in tokens {
        match token {
            Token::Word(w) => {
                let expanded = expand_token(w.clone(), last_status);
                words.push(strip_markers(&expanded));
            }
            Token::Redirect(RedirectKind::Stdout) => words.push(">".to_string()),
            Token::Redirect(RedirectKind::Stdin) => words.push("<".to_string()),
            Token::Redirect(RedirectKind::StdoutAppend) => words.push(">>".to_string()),
            _ => return Err(format!("unexpected token in structured pipe: {token:?}")),
        }
    }

    if words.is_empty() {
        return Err("empty structured pipe stage".into());
    }

    let first = &words[0];
    if first.starts_with('.') {
        return Ok(StructuredOp::Project(first.clone()));
    }

    match first.as_str() {
        "count" => Ok(StructuredOp::Count),
        "json" => Ok(StructuredOp::Format(OutputFormat::Json)),
        "yaml" => Ok(StructuredOp::Format(OutputFormat::Yaml)),
        "toml" => Ok(StructuredOp::Format(OutputFormat::Toml)),
        "table" => Ok(StructuredOp::Format(OutputFormat::Table)),
        "take" => {
            if words.len() < 2 {
                return Err("take requires a count".into());
            }
            let n = words[1]
                .parse::<usize>()
                .map_err(|_| "invalid number for take")?;
            Ok(StructuredOp::Take(n))
        }
        "skip" => {
            if words.len() < 2 {
                return Err("skip requires a count".into());
            }
            let n = words[1]
                .parse::<usize>()
                .map_err(|_| "invalid number for skip")?;
            Ok(StructuredOp::Skip(n))
        }
        "filter" => {
            let expr_str = words[1..].join(" ");
            let filter_expr = parse_filter_expr(&expr_str)?;
            Ok(StructuredOp::Filter(filter_expr))
        }
        _ => Err(format!("unknown structured operation: {first}")),
    }
}

fn parse_filter_expr(s: &str) -> Result<FilterExpr, String> {
    let trimmed = s.trim();
    let inner = if trimmed.starts_with('(') && trimmed.ends_with(')') {
        trimmed[1..trimmed.len() - 1].trim()
    } else {
        trimmed
    };

    let normalized = inner.replace("> =", ">=").replace("< =", "<=");

    let ops = [
        ("==", FilterOp::Equal),
        ("!=", FilterOp::NotEqual),
        (">=", FilterOp::GreaterOrEqual),
        ("<=", FilterOp::LessOrEqual),
        (">", FilterOp::GreaterThan),
        ("<", FilterOp::LessThan),
    ];

    for (op_str, op_enum) in ops {
        if let Some(idx) = normalized.find(op_str) {
            let left = normalized[..idx].trim();
            let right = normalized[idx + op_str.len()..].trim();
            let clean_right = right.trim_matches('"').trim_matches('\'');
            return Ok(FilterExpr {
                field: left.to_string(),
                op: op_enum,
                value: clean_right.to_string(),
            });
        }
    }

    Ok(FilterExpr {
        field: normalized.to_string(),
        op: FilterOp::Exists,
        value: String::new(),
    })
}

fn parse_if(tokens: &[Token], last_status: i32) -> Result<ParsedIf, String> {
    let then_index = tokens
        .iter()
        .position(|t| matches!(t, Token::Then))
        .ok_or_else(|| "missing 'then' in if statement".to_string())?;

    let condition = parse_pipeline_from_tokens(&tokens[1..then_index], last_status)?;

    let else_index = tokens.iter().position(|t| matches!(t, Token::Else));
    let fi_index = tokens
        .iter()
        .position(|t| matches!(t, Token::Fi))
        .ok_or_else(|| "missing 'fi' in if statement".to_string())?;

    let then_end = else_index.unwrap_or(fi_index);
    let then_branch_tokens = &tokens[then_index + 1..then_end];
    let then_branch = parse_branch(then_branch_tokens, last_status)?;

    let else_branch = if let Some(e_idx) = else_index {
        let else_branch_tokens = &tokens[e_idx + 1..fi_index];
        Some(parse_branch(else_branch_tokens, last_status)?)
    } else {
        None
    };

    Ok(ParsedIf {
        condition,
        then_branch,
        else_branch,
    })
}


fn parse_for(tokens: &[Token], _last_status: i32) -> Result<ParsedFor, String> {
    let mut i = 1;
    if i >= tokens.len() {
        return Err("for: expected variable name".into());
    }
    let variable = match &tokens[i] {
        Token::Word(w) => w.clone(),
        _ => return Err("for: expected variable name".into()),
    };
    i += 1;
    
    let mut values = Vec::new();
    if i < tokens.len()
        && let Token::Word(w) = &tokens[i]
            && w == "in" {
                i += 1;
                while i < tokens.len() {
                    match &tokens[i] {
                        Token::Semicolon => break,
                        Token::Word(s) if s == "do" => break,
                        Token::Word(w) => values.push(w.clone()),
                        _ => break,
                    }
                    i += 1;
                }
            }
    
    // consume optional semicolon
    if i < tokens.len() && matches!(&tokens[i], Token::Semicolon) {
        i += 1;
    }
    
    if i >= tokens.len() || !matches!(&tokens[i], Token::Word(w) if w == "do") {
        return Err("for: expected 'do'".into());
    }
    i += 1; // skip 'do'
    
    let mut body_tokens = Vec::new();
    let mut depth = 1;
    while i < tokens.len() {
        if let Token::Word(w) = &tokens[i] {
            if w == "for" || w == "while" || w == "if" {
                depth += 1;
            } else if w == "done" || w == "fi" {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
        }
        body_tokens.push(tokens[i].clone());
        i += 1;
    }
    
    if i >= tokens.len() || depth > 0 {
        return Err("for: expected 'done'".into());
    }
    
    Ok(ParsedFor { variable, values, body_tokens })
}

fn parse_while(tokens: &[Token], _last_status: i32) -> Result<ParsedWhile, String> {
    let mut i = 1;
    let mut condition_tokens = Vec::new();
    
    while i < tokens.len() {
        match &tokens[i] {
            Token::Semicolon => break,
            Token::Word(w) if w == "do" => break,
            _ => condition_tokens.push(tokens[i].clone()),
        }
        i += 1;
    }
    
    // consume optional semicolon
    if i < tokens.len() && matches!(&tokens[i], Token::Semicolon) {
        i += 1;
    }
    
    if i >= tokens.len() || !matches!(&tokens[i], Token::Word(w) if w == "do") {
        return Err("while: expected 'do'".into());
    }
    i += 1; // skip 'do'
    
    let mut body_tokens = Vec::new();
    let mut depth = 1;
    while i < tokens.len() {
        if let Token::Word(w) = &tokens[i] {
            if w == "while" || w == "for" || w == "if" {
                depth += 1;
            } else if w == "done" || w == "fi" {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
        }
        body_tokens.push(tokens[i].clone());
        i += 1;
    }
    
    if i >= tokens.len() || depth > 0 {
        return Err("while: expected 'done'".into());
    }
    
    Ok(ParsedWhile { condition_tokens, body_tokens })
}

fn try_parse_function(tokens: &[Token], _last_status: i32) -> Result<Option<ParsedFunction>, String> {
    if tokens.is_empty() {
        return Ok(None);
    }
    let mut i = 0;
    let name: String;

    if let Token::Word(w) = &tokens[0] {
        if w == "function" {
            i += 1;
            if i >= tokens.len() {
                return Err("function: expected function name".into());
            }
            if let Token::Word(func_name) = &tokens[i] {
                if func_name.ends_with("()") && func_name.len() > 2 {
                    name = func_name[..func_name.len() - 2].to_string();
                } else {
                    name = func_name.clone();
                }
                i += 1;
                if i < tokens.len()
                    && let Token::Word(paren) = &tokens[i]
                        && paren == "()" {
                            i += 1;
                        }
            } else {
                return Err("function: expected function name".into());
            }
        } else if w.ends_with("()") && w.len() > 2 {
            name = w[..w.len() - 2].to_string();
            i += 1;
        } else if i + 1 < tokens.len() {
            if let Token::Word(paren) = &tokens[1] {
                if paren == "()" && is_valid_variable_name(w) {
                    name = w.clone();
                    i += 2;
                } else {
                    return Ok(None);
                }
            } else {
                return Ok(None);
            }
        } else {
            return Ok(None);
        }
    } else {
        return Ok(None);
    }

    if !is_valid_variable_name(&name) {
        return Err(format!("function: invalid function name '{name}'"));
    }

    let mut found_brace = false;
    while i < tokens.len() {
        match &tokens[i] {
            Token::Word(w) if w == "{" => {
                i += 1;
                found_brace = true;
                break;
            }
            Token::Newline => {
                i += 1;
            }
            _ => break,
        }
    }

    if !found_brace {
        return Ok(None);
    }

    let mut body_tokens = Vec::new();
    let mut depth = 1;
    while i < tokens.len() {
        if let Token::Word(w) = &tokens[i] {
            if w == "{" {
                depth += 1;
            } else if w == "}" {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
        }
        body_tokens.push(tokens[i].clone());
        i += 1;
    }

    if depth > 0 {
        return Err("function: expected '}'".into());
    }

    Ok(Some(ParsedFunction { name, body_tokens }))
}

pub fn parse_branch(tokens: &[Token], last_status: i32) -> Result<Vec<Ast>, String> {
    if tokens.is_empty() {
        return Ok(Vec::new());
    }
    match parse_ast_from_tokens(tokens, last_status)? {
        Some(Ast::Sequence(seq)) => Ok(seq.items.into_iter().map(|item| item.node).collect()),
        Some(ast) => Ok(vec![ast]),
        None => Ok(Vec::new()),
    }
}

#[allow(dead_code)]
pub fn parse_pipeline(input: &str) -> Result<Option<ParsedPipeline>, String> {
    parse_pipeline_with_status(input, 0)
}

#[allow(dead_code)]
pub fn parse_pipeline_with_status(
    input: &str,
    last_status: i32,
) -> Result<Option<ParsedPipeline>, String> {
    let tokens = tokenize(input)?;
    if tokens.is_empty() {
        return Ok(None);
    }
    Ok(Some(parse_pipeline_from_tokens(&tokens, last_status)?))
}

fn parse_pipeline_from_tokens(
    tokens: &[Token],
    last_status: i32,
) -> Result<ParsedPipeline, String> {
    let mut commands = Vec::new();
    let mut words = Vec::new();
    let mut redirects = Vec::new();
    let mut background = false;
    let mut index = 0;

    while index < tokens.len() {
        match &tokens[index] {
            Token::Word(word) => words.push(word.clone()),
            Token::Pipe => {
                commands.push(build_command(&mut words, &mut redirects, last_status)?);
            }
            Token::Background => {
                if index + 1 != tokens.len() {
                    return Err("background marker must be at the end of a command".into());
                }
                if words.is_empty() && commands.is_empty() {
                    return Err("background marker requires a command".into());
                }
                background = true;
            }
            Token::Redirect(kind) => match kind {
                RedirectKind::DupRead(src, dst) => {
                    redirects.push(Redirection::DupRead(*src, *dst));
                }
                RedirectKind::DupWrite(src, dst) => {
                    redirects.push(Redirection::DupWrite(*src, *dst));
                }
                _ => {
                    index += 1;
                    let target = match tokens.get(index) {
                        Some(Token::Word(target)) => {
                            strip_markers(&expand_token(target.clone(), last_status))
                        }
                        _ => return Err("redirection requires a target".into()),
                    };
                    redirects.push(match kind {
                        RedirectKind::Stdin => Redirection::Stdin(target),
                        RedirectKind::Stdout => Redirection::Stdout(target),
                        RedirectKind::StdoutAppend => Redirection::StdoutAppend(target),
                        RedirectKind::Stderr => Redirection::Stderr(target),
                        RedirectKind::StderrAppend => Redirection::StderrAppend(target),
                        RedirectKind::StdoutAndStderr => Redirection::StdoutAndStderr(target),
                        RedirectKind::StdoutAndStderrAppend => {
                            Redirection::StdoutAndStderrAppend(target)
                        }
                        RedirectKind::HereString => Redirection::HereString(target),
                        RedirectKind::DupRead(s, d) => Redirection::DupRead(*s, *d),
                        RedirectKind::DupWrite(s, d) => Redirection::DupWrite(*s, *d),
                    });
                }
            },
            Token::Semicolon | Token::Newline => {}
            _ => return Err(format!("unexpected token: {:?}", tokens[index])),
        }
        index += 1;
    }

    commands.push(build_command(&mut words, &mut redirects, last_status)?);
    Ok(ParsedPipeline {
        commands,
        background,
    })
}

fn build_command(
    words: &mut Vec<String>,
    redirects: &mut Vec<Redirection>,
    last_status: i32,
) -> Result<ParsedCommand, String> {
    if words.is_empty() {
        return Err("expected command before pipe".into());
    }

    let words = expand_tokens(std::mem::take(words), last_status);
    if words.is_empty() {
        return Err("expected command after expansion".into());
    }

    let assignment_count = words
        .iter()
        .take_while(|word| {
            word.split_once('=')
                .is_some_and(|(name, _)| is_valid_variable_name(name))
        })
        .count();
    if assignment_count == words.len() && assignment_count > 0 {
        let mut export_words = vec!["export".to_string()];
        export_words.extend(words.clone());
        return Ok(ParsedCommand {
            program: "export".to_string(),
            args: export_words[1..].to_vec(),
            environment: vec![],
            redirects: std::mem::take(redirects),
        });
    }

    let environment = words[..assignment_count]
        .iter()
        .map(|assignment| {
            assignment
                .split_once('=')
                .expect("assignment count only includes assignments")
        })
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
    let program = words[assignment_count].clone();
    Ok(ParsedCommand {
        program,
        args: words[assignment_count + 1..].to_vec(),
        environment,
        redirects: std::mem::take(redirects),
    })
}

pub(crate) fn is_valid_variable_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_alphabetic() || first == '_')
        && chars.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

pub fn strip_markers(s: &str) -> String {
    s.chars()
        .filter(|character| {
            !matches!(
                *character,
                LITERAL_START
                    | LITERAL_END
                    | ESCAPED_START
                    | ESCAPED_END
                    | DOUBLE_QUOTE_MARKER
            )
        })
        .collect()
}

pub fn expand_tokens(words: Vec<String>, last_status: i32) -> Vec<String> {
    let mut expanded = Vec::new();
    for word in words {
        let single_expanded = expand_token(word, last_status);
        let globs = expand_globs(&single_expanded);
        for g in globs {
            expanded.push(strip_markers(&g));
        }
    }
    expanded
}

pub fn expand_token(token: String, last_status: i32) -> String {
    let home = env::var("HOME").unwrap_or_default();

    // 1. Tilde expansion
    let expanded = if token == "~" {
        home.clone()
    } else if let Some(rest) = token.strip_prefix("~/") {
        format!("{home}/{rest}")
    } else {
        token
    };

    // 2. Command substitution $(...) and `...`
    let with_cmd_sub = expand_command_substitution(&expanded, last_status);

    // 3. Arithmetic expansion $((...))
    let with_arith = expand_arithmetic(&with_cmd_sub);

    // 4. Parameter / environment variable expansion
    expand_environment_variables(&with_arith, last_status)
}

fn expand_command_substitution(input: &str, last_status: i32) -> String {
    let mut result = String::new();
    let mut chars = input.chars().peekable();
    let mut in_single_quote = false;

    while let Some(ch) = chars.next() {
        if ch == LITERAL_START {
            in_single_quote = true;
            result.push(ch);
            continue;
        }
        if ch == LITERAL_END {
            in_single_quote = false;
            result.push(ch);
            continue;
        }
        if in_single_quote {
            result.push(ch);
            continue;
        }

        if ch == '$' && chars.peek() == Some(&'(') {
            chars.next();
            if chars.peek() == Some(&'(') {
                result.push('$');
                result.push('(');
                result.push('(');
                chars.next();
                continue;
            }

            let mut sub_cmd = String::new();
            let mut depth = 1;
            for next in chars.by_ref() {
                if next == '(' {
                    depth += 1;
                } else if next == ')' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                sub_cmd.push(next);
            }

            match execute_subshell(&sub_cmd, last_status) {
                Ok(output) => result.push_str(&output),
                Err(err) => {
                    eprintln!("shellpilot: command substitution error: {err}");
                }
            }
            continue;
        }

        if ch == '`' {
            let mut sub_cmd = String::new();
            let mut escaped = false;
            for next in chars.by_ref() {
                if escaped {
                    sub_cmd.push(next);
                    escaped = false;
                    continue;
                }
                if next == '\\' {
                    escaped = true;
                    continue;
                }
                if next == '`' {
                    break;
                }
                sub_cmd.push(next);
            }

            match execute_subshell(&sub_cmd, last_status) {
                Ok(output) => result.push_str(&output),
                Err(err) => {
                    eprintln!("shellpilot: command substitution error: {err}");
                }
            }
            continue;
        }

        result.push(ch);
    }

    result
}

fn expand_arithmetic(input: &str) -> String {
    let mut result = String::new();
    let mut chars = input.chars().peekable();
    let mut in_single_quote = false;

    while let Some(ch) = chars.next() {
        if ch == LITERAL_START {
            in_single_quote = true;
            result.push(ch);
            continue;
        }
        if ch == LITERAL_END {
            in_single_quote = false;
            result.push(ch);
            continue;
        }
        if in_single_quote {
            result.push(ch);
            continue;
        }

        if ch == '$' && chars.peek() == Some(&'(') {
            let mut clone_chars = chars.clone();
            clone_chars.next();
            if clone_chars.peek() == Some(&'(') {
                chars.next();
                chars.next();
                let mut expr = String::new();
                let mut depth = 2;
                while let Some(next) = chars.next() {
                    if next == '(' {
                        depth += 1;
                    } else if next == ')' {
                        depth -= 1;
                        if depth == 1 && chars.peek() == Some(&')') {
                            chars.next();
                            break;
                        }
                    }
                    expr.push(next);
                }

                match evaluate_arithmetic(&expr) {
                    Ok(val) => result.push_str(&val.to_string()),
                    Err(err) => {
                        eprintln!("shellpilot: arithmetic error: {err}");
                        result.push('0');
                    }
                }
                continue;
            }
        }

        result.push(ch);
    }

    result
}

pub fn evaluate_arithmetic(expr: &str) -> Result<i64, String> {
    let tokens = tokenize_arithmetic(expr)?;
    let mut pos = 0;
    parse_arith_logical_or(&tokens, &mut pos)
}

#[derive(Debug, Clone, PartialEq)]
enum ArithToken {
    Num(i64),
    Var(String),
    Plus,
    Minus,
    Mul,
    Div,
    Mod,
    Eq,
    Neq,
    Lt,
    Lte,
    Gt,
    Gte,
    And,
    Or,
    Not,
    LParen,
    RParen,
}

fn tokenize_arithmetic(expr: &str) -> Result<Vec<ArithToken>, String> {
    let mut tokens = Vec::new();
    let mut chars = expr.chars().peekable();

    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }

        if c.is_ascii_digit() {
            let mut num_str = String::new();
            while let Some(&d) = chars.peek() {
                if d.is_ascii_digit() {
                    num_str.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
            let val = num_str.parse::<i64>().map_err(|e| e.to_string())?;
            tokens.push(ArithToken::Num(val));
            continue;
        }

        if c.is_ascii_alphabetic() || c == '_' || c == '$' {
            let mut name = String::new();
            if c == '$' {
                chars.next();
            }
            while let Some(&d) = chars.peek() {
                if d.is_ascii_alphanumeric() || d == '_' {
                    name.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
            tokens.push(ArithToken::Var(name));
            continue;
        }

        chars.next();
        match c {
            '+' => tokens.push(ArithToken::Plus),
            '-' => tokens.push(ArithToken::Minus),
            '*' => tokens.push(ArithToken::Mul),
            '/' => tokens.push(ArithToken::Div),
            '%' => tokens.push(ArithToken::Mod),
            '(' => tokens.push(ArithToken::LParen),
            ')' => tokens.push(ArithToken::RParen),
            '!' => {
                if chars.peek() == Some(&'=') {
                    chars.next();
                    tokens.push(ArithToken::Neq);
                } else {
                    tokens.push(ArithToken::Not);
                }
            }
            '=' => {
                if chars.peek() == Some(&'=') {
                    chars.next();
                    tokens.push(ArithToken::Eq);
                } else {
                    return Err("unexpected assignment in arithmetic".into());
                }
            }
            '<' => {
                if chars.peek() == Some(&'=') {
                    chars.next();
                    tokens.push(ArithToken::Lte);
                } else {
                    tokens.push(ArithToken::Lt);
                }
            }
            '>' => {
                if chars.peek() == Some(&'=') {
                    chars.next();
                    tokens.push(ArithToken::Gte);
                } else {
                    tokens.push(ArithToken::Gt);
                }
            }
            '&' => {
                if chars.peek() == Some(&'&') {
                    chars.next();
                    tokens.push(ArithToken::And);
                } else {
                    return Err("unexpected '&' in arithmetic".into());
                }
            }
            '|' => {
                if chars.peek() == Some(&'|') {
                    chars.next();
                    tokens.push(ArithToken::Or);
                } else {
                    return Err("unexpected '|' in arithmetic".into());
                }
            }
            other => return Err(format!("unexpected character in arithmetic: {other}")),
        }
    }

    Ok(tokens)
}

fn parse_arith_logical_or(tokens: &[ArithToken], pos: &mut usize) -> Result<i64, String> {
    let mut left = parse_arith_logical_and(tokens, pos)?;
    while *pos < tokens.len() && tokens[*pos] == ArithToken::Or {
        *pos += 1;
        let right = parse_arith_logical_and(tokens, pos)?;
        left = if left != 0 || right != 0 { 1 } else { 0 };
    }
    Ok(left)
}

fn parse_arith_logical_and(tokens: &[ArithToken], pos: &mut usize) -> Result<i64, String> {
    let mut left = parse_arith_equality(tokens, pos)?;
    while *pos < tokens.len() && tokens[*pos] == ArithToken::And {
        *pos += 1;
        let right = parse_arith_equality(tokens, pos)?;
        left = if left != 0 && right != 0 { 1 } else { 0 };
    }
    Ok(left)
}

fn parse_arith_equality(tokens: &[ArithToken], pos: &mut usize) -> Result<i64, String> {
    let mut left = parse_arith_relational(tokens, pos)?;
    while *pos < tokens.len() {
        match tokens[*pos] {
            ArithToken::Eq => {
                *pos += 1;
                let right = parse_arith_relational(tokens, pos)?;
                left = if left == right { 1 } else { 0 };
            }
            ArithToken::Neq => {
                *pos += 1;
                let right = parse_arith_relational(tokens, pos)?;
                left = if left != right { 1 } else { 0 };
            }
            _ => break,
        }
    }
    Ok(left)
}

fn parse_arith_relational(tokens: &[ArithToken], pos: &mut usize) -> Result<i64, String> {
    let mut left = parse_arith_additive(tokens, pos)?;
    while *pos < tokens.len() {
        match tokens[*pos] {
            ArithToken::Lt => {
                *pos += 1;
                let right = parse_arith_additive(tokens, pos)?;
                left = if left < right { 1 } else { 0 };
            }
            ArithToken::Lte => {
                *pos += 1;
                let right = parse_arith_additive(tokens, pos)?;
                left = if left <= right { 1 } else { 0 };
            }
            ArithToken::Gt => {
                *pos += 1;
                let right = parse_arith_additive(tokens, pos)?;
                left = if left > right { 1 } else { 0 };
            }
            ArithToken::Gte => {
                *pos += 1;
                let right = parse_arith_additive(tokens, pos)?;
                left = if left >= right { 1 } else { 0 };
            }
            _ => break,
        }
    }
    Ok(left)
}

fn parse_arith_additive(tokens: &[ArithToken], pos: &mut usize) -> Result<i64, String> {
    let mut left = parse_arith_multiplicative(tokens, pos)?;
    while *pos < tokens.len() {
        match tokens[*pos] {
            ArithToken::Plus => {
                *pos += 1;
                let right = parse_arith_multiplicative(tokens, pos)?;
                left = left.wrapping_add(right);
            }
            ArithToken::Minus => {
                *pos += 1;
                let right = parse_arith_multiplicative(tokens, pos)?;
                left = left.wrapping_sub(right);
            }
            _ => break,
        }
    }
    Ok(left)
}

fn parse_arith_multiplicative(tokens: &[ArithToken], pos: &mut usize) -> Result<i64, String> {
    let mut left = parse_arith_unary(tokens, pos)?;
    while *pos < tokens.len() {
        match tokens[*pos] {
            ArithToken::Mul => {
                *pos += 1;
                let right = parse_arith_unary(tokens, pos)?;
                left = left.wrapping_mul(right);
            }
            ArithToken::Div => {
                *pos += 1;
                let right = parse_arith_unary(tokens, pos)?;
                if right == 0 {
                    return Err("division by zero".into());
                }
                left /= right;
            }
            ArithToken::Mod => {
                *pos += 1;
                let right = parse_arith_unary(tokens, pos)?;
                if right == 0 {
                    return Err("modulo by zero".into());
                }
                left %= right;
            }
            _ => break,
        }
    }
    Ok(left)
}

fn parse_arith_unary(tokens: &[ArithToken], pos: &mut usize) -> Result<i64, String> {
    if *pos >= tokens.len() {
        return Err("unexpected end of arithmetic expression".into());
    }
    match tokens[*pos] {
        ArithToken::Plus => {
            *pos += 1;
            parse_arith_unary(tokens, pos)
        }
        ArithToken::Minus => {
            *pos += 1;
            let val = parse_arith_unary(tokens, pos)?;
            Ok(-val)
        }
        ArithToken::Not => {
            *pos += 1;
            let val = parse_arith_unary(tokens, pos)?;
            Ok(if val == 0 { 1 } else { 0 })
        }
        _ => parse_arith_primary(tokens, pos),
    }
}

fn parse_arith_primary(tokens: &[ArithToken], pos: &mut usize) -> Result<i64, String> {
    if *pos >= tokens.len() {
        return Err("unexpected end of arithmetic expression".into());
    }
    match &tokens[*pos] {
        ArithToken::Num(n) => {
            let val = *n;
            *pos += 1;
            Ok(val)
        }
        ArithToken::Var(name) => {
            let val = env::var(name)
                .ok()
                .and_then(|v| v.trim().parse::<i64>().ok())
                .unwrap_or(0);
            *pos += 1;
            Ok(val)
        }
        ArithToken::LParen => {
            *pos += 1;
            let val = parse_arith_logical_or(tokens, pos)?;
            if *pos >= tokens.len() || tokens[*pos] != ArithToken::RParen {
                return Err("missing closing parenthesis in arithmetic".into());
            }
            *pos += 1;
            Ok(val)
        }
        other => Err(format!("unexpected token in arithmetic: {other:?}")),
    }
}

fn expand_environment_variables(input: &str, last_status: i32) -> String {
    let mut result = String::new();
    let mut chars = input.chars().peekable();
    let mut literal = false;

    while let Some(ch) = chars.next() {
        if matches!(ch, LITERAL_START | ESCAPED_START) {
            literal = true;
            result.push(ch);
            continue;
        }
        if matches!(ch, LITERAL_END | ESCAPED_END) {
            literal = false;
            result.push(ch);
            continue;
        }
        if ch == DOUBLE_QUOTE_MARKER {
            result.push(ch);
            continue;
        }
        if literal {
            result.push(ch);
            continue;
        }
        if ch != '$' {
            result.push(ch);
            continue;
        }

        match chars.peek() {
            Some('?') => {
                chars.next();
                result.push_str(&last_status.to_string());
                continue;
            }
            Some('$') => {
                chars.next();
                result.push_str(&std::process::id().to_string());
                continue;
            }
            Some('#') => {
                chars.next();
                let params = get_positional_params();
                let count = params.len().saturating_sub(1);
                result.push_str(&count.to_string());
                continue;
            }
            Some('*') | Some('@') => {
                chars.next();
                let params = get_positional_params();
                if params.len() > 1 {
                    result.push_str(&params[1..].join(" "));
                }
                continue;
            }
            Some(&d) if d.is_ascii_digit() => {
                chars.next();
                let index = d.to_digit(10).unwrap() as usize;
                let params = get_positional_params();
                if index < params.len() {
                    result.push_str(&params[index]);
                } else if index == 0 {
                    result.push_str("shellpilot");
                }
                continue;
            }
            Some('{') => {
                chars.next();
                let mut expr = String::new();
                for next in chars.by_ref() {
                    if next == '}' {
                        break;
                    }
                    expr.push(next);
                }
                result.push_str(&resolve_parameter_expression(&expr, last_status));
                continue;
            }
            Some(next) if next.is_ascii_alphabetic() || *next == '_' => {
                let mut name = String::new();
                while let Some(next) = chars.peek().copied() {
                    if next.is_ascii_alphanumeric() || next == '_' {
                        name.push(chars.next().unwrap());
                    } else {
                        break;
                    }
                }
                result.push_str(&env::var(&name).unwrap_or_default());
            }
            _ => {
                result.push('$');
                continue;
            }
        }
    }

    result
}

fn resolve_parameter_expression(expr: &str, last_status: i32) -> String {
    if let Some(var) = expr.strip_prefix('#') {
        if var == "?" {
            return last_status.to_string().len().to_string();
        }
        let val = env::var(var).unwrap_or_default();
        return val.chars().count().to_string();
    }

    if let Some((var, default)) = expr.split_once(":-") {
        let val = env::var(var).unwrap_or_default();
        if val.is_empty() {
            return default.to_string();
        }
        return val;
    }

    if let Some((var, default)) = expr.split_once(":=") {
        let val = env::var(var).unwrap_or_default();
        if val.is_empty() {
            unsafe { env::set_var(var, default) };
            return default.to_string();
        }
        return val;
    }

    if let Some((var, alt)) = expr.split_once(":+") {
        let val = env::var(var).unwrap_or_default();
        if !val.is_empty() {
            return alt.to_string();
        }
        return String::new();
    }

    if expr == "?" {
        return last_status.to_string();
    }
    if expr == "$" {
        return std::process::id().to_string();
    }
    if expr == "#" {
        let params = get_positional_params();
        return params.len().saturating_sub(1).to_string();
    }
    if let Ok(idx) = expr.parse::<usize>() {
        let params = get_positional_params();
        if idx < params.len() {
            return params[idx].clone();
        } else if idx == 0 {
            return "shellpilot".to_string();
        }
        return String::new();
    }

    env::var(expr).unwrap_or_default()
}

pub fn expand_globs(word: &str) -> Vec<String> {
    let mut has_unquoted_glob = false;
    let mut literal = false;
    for c in word.chars() {
        if matches!(c, LITERAL_START | ESCAPED_START) {
            literal = true;
            continue;
        }
        if matches!(c, LITERAL_END | ESCAPED_END) {
            literal = false;
            continue;
        }
        if c == DOUBLE_QUOTE_MARKER {
            continue;
        }
        if !literal && matches!(c, '*' | '?' | '[') {
            has_unquoted_glob = true;
            break;
        }
    }

    if !has_unquoted_glob {
        return vec![word.to_string()];
    }

    let (dir_str, file_pattern) = match word.rsplit_once('/') {
        Some((d, f)) => (if d.is_empty() { "/" } else { d }, f),
        None => (".", word),
    };

    let clean_dir = dir_str
        .chars()
        .filter(|c| {
            !matches!(
                *c,
                LITERAL_START | LITERAL_END | ESCAPED_START | ESCAPED_END | DOUBLE_QUOTE_MARKER
            )
        })
        .collect::<String>();

    let dir_path = Path::new(&clean_dir);
    let entries = match fs::read_dir(dir_path) {
        Ok(e) => e,
        Err(_) => return vec![word.to_string()],
    };

    let mut matches = Vec::new();
    let pattern_clean = file_pattern
        .chars()
        .filter(|c| {
            !matches!(
                *c,
                LITERAL_START | LITERAL_END | ESCAPED_START | ESCAPED_END | DOUBLE_QUOTE_MARKER
            )
        })
        .collect::<String>();

    let show_hidden = pattern_clean.starts_with('.');

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        if glob_match(&pattern_clean, &name) {
            let path_str = if dir_str == "." {
                name
            } else if dir_str == "/" {
                format!("/{name}")
            } else {
                format!("{clean_dir}/{name}")
            };
            matches.push(path_str);
        }
    }

    if matches.is_empty() {
        vec![word.to_string()]
    } else {
        matches.sort();
        matches
    }
}

pub fn glob_match(pattern: &str, text: &str) -> bool {
    let mut p = pattern.chars().peekable();
    let mut t = text.chars().peekable();

    match (p.peek().copied(), t.peek().copied()) {
        (None, None) => true,
        (Some('*'), _) => {
            p.next();
            while p.peek() == Some(&'*') {
                p.next();
            }
            let remaining_pattern: String = p.collect();
            for i in 0..=text.len() {
                if text.is_char_boundary(i) && glob_match(&remaining_pattern, &text[i..]) {
                    return true;
                }
            }
            false
        }
        (Some('?'), Some(_)) => {
            p.next();
            t.next();
            let p_rem: String = p.collect();
            let t_rem: String = t.collect();
            glob_match(&p_rem, &t_rem)
        }
        (Some('['), Some(tc)) => {
            p.next();
            let mut matched = false;
            let mut invert = false;
            if p.peek() == Some(&'!') || p.peek() == Some(&'^') {
                invert = true;
                p.next();
            }
            while let Some(c) = p.next() {
                if c == ']' {
                    break;
                }
                if p.peek() == Some(&'-') {
                    p.next();
                    if let Some(end) = p.next()
                        && tc >= c && tc <= end {
                            matched = true;
                        }
                } else if c == tc {
                    matched = true;
                }
            }
            if invert {
                matched = !matched;
            }
            if !matched {
                return false;
            }
            t.next();
            let p_rem: String = p.collect();
            let t_rem: String = t.collect();
            glob_match(&p_rem, &t_rem)
        }
        (Some(pc), Some(tc)) if pc == tc => {
            p.next();
            t.next();
            let p_rem: String = p.collect();
            let t_rem: String = t.collect();
            glob_match(&p_rem, &t_rem)
        }
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    Word(String),
    Pipe,
    StructuredPipe,
    Background,
    LogicalAnd,
    LogicalOr,
    Semicolon,
    Newline,
    Redirect(RedirectKind),
    If,
    Then,
    Else,
    Fi,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedirectKind {
    Stdin,
    Stdout,
    StdoutAppend,
    Stderr,
    StderrAppend,
    StdoutAndStderr,
    StdoutAndStderrAppend,
    DupRead(i32, i32),
    DupWrite(i32, i32),
    HereString,
}

fn tokenize(input: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut current = String::new();

    let mut quote: Option<char> = None;
    let mut paren_depth: usize = 0;
    let mut subshell_quote: Option<char> = None;
    let mut in_backtick = false;
    let mut escaped = false;
    let mut saw_argument = false;
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if escaped {
            if quote == Some('"') && !matches!(c, '$' | '`' | '"' | '\\' | '\n') {
                current.push('\\');
                current.push(c);
            } else {
                current.push(ESCAPED_START);
                current.push(c);
                current.push(ESCAPED_END);
            }
            escaped = false;
            saw_argument = true;
            continue;
        }

        if c == '\\' && quote != Some('\'') {
            escaped = true;
            saw_argument = true;
            continue;
        }

        if let Some(active_quote) = quote {
            if c == active_quote {
                if active_quote == '\'' {
                    current.push(LITERAL_END);
                }
                quote = None;
            } else {
                current.push(c);
            }
            saw_argument = true;
            continue;
        }

        // Handle paren_depth inside $(...) or $((...))
        if paren_depth > 0 {
            saw_argument = true;
            if let Some(sq) = subshell_quote {
                if c == sq {
                    subshell_quote = None;
                }
                current.push(c);
                continue;
            }
            if c == '"' || c == '\'' {
                subshell_quote = Some(c);
                current.push(c);
                continue;
            }
            if c == '(' {
                paren_depth += 1;
                current.push(c);
                continue;
            }
            if c == ')' {
                paren_depth -= 1;
                current.push(c);
                continue;
            }
            current.push(c);
            continue;
        }

        // Handle backtick command substitution
        if in_backtick {
            saw_argument = true;
            current.push(c);
            if c == '`' {
                in_backtick = false;
            }
            continue;
        }

        if c == '`' {
            saw_argument = true;
            in_backtick = true;
            current.push(c);
            continue;
        }

        if c == '$' && chars.peek() == Some(&'(') {
            saw_argument = true;
            current.push('$');
            current.push('(');
            chars.next();
            paren_depth = 1;
            if chars.peek() == Some(&'(') {
                current.push('(');
                chars.next();
                paren_depth = 2;
            }
            continue;
        }

        if c == ';' {
            if saw_argument {
                tokens.push(classify_word(std::mem::take(&mut current)));
                saw_argument = false;
            }
            tokens.push(Token::Semicolon);
            continue;
        }

        if c == '\n' {
            if saw_argument {
                tokens.push(classify_word(std::mem::take(&mut current)));
                saw_argument = false;
            }
            tokens.push(Token::Newline);
            continue;
        }

        if c == '|' || c == '&' || c == '<' || c == '>' {
            if c == '|' {
                if saw_argument {
                    tokens.push(classify_word(std::mem::take(&mut current)));
                    saw_argument = false;
                }
                if chars.peek() == Some(&'|') {
                    chars.next();
                    tokens.push(Token::LogicalOr);
                } else if chars.peek() == Some(&'>') {
                    chars.next();
                    tokens.push(Token::StructuredPipe);
                } else {
                    tokens.push(Token::Pipe);
                }
                continue;
            }

            if c == '&' {
                if saw_argument {
                    tokens.push(classify_word(std::mem::take(&mut current)));
                    saw_argument = false;
                }
                if chars.peek() == Some(&'&') {
                    chars.next();
                    tokens.push(Token::LogicalAnd);
                } else if chars.peek() == Some(&'>') {
                    chars.next();
                    if chars.peek() == Some(&'>') {
                        chars.next();
                        tokens.push(Token::Redirect(RedirectKind::StdoutAndStderrAppend));
                    } else {
                        tokens.push(Token::Redirect(RedirectKind::StdoutAndStderr));
                    }
                } else {
                    tokens.push(Token::Background);
                }
                continue;
            }

            if c == '<' {
                if saw_argument {
                    tokens.push(classify_word(std::mem::take(&mut current)));
                    saw_argument = false;
                }
                if chars.peek() == Some(&'<') {
                    chars.next();
                    if chars.peek() == Some(&'<') {
                        chars.next();
                        tokens.push(Token::Redirect(RedirectKind::HereString));
                    } else {
                        tokens.push(Token::Redirect(RedirectKind::Stdin));
                    }
                } else if chars.peek() == Some(&'&') {
                    chars.next();
                    let target = if let Some(&d) = chars.peek() {
                        if d.is_ascii_digit() {
                            chars.next();
                            d.to_digit(10).unwrap() as i32
                        } else {
                            0
                        }
                    } else {
                        0
                    };
                    tokens.push(Token::Redirect(RedirectKind::DupRead(0, target)));
                } else {
                    tokens.push(Token::Redirect(RedirectKind::Stdin));
                }
                continue;
            }

            if c == '>' {
                let stderr = current == "2";
                let stdout = current == "1";
                if stderr || stdout {
                    let from_fd = if stderr { 2 } else { 1 };
                    current.clear();
                    saw_argument = false;
                    if chars.peek() == Some(&'>') {
                        chars.next();
                        tokens.push(Token::Redirect(if stderr {
                            RedirectKind::StderrAppend
                        } else {
                            RedirectKind::StdoutAppend
                        }));
                    } else if chars.peek() == Some(&'&') {
                        chars.next();
                        let target = if let Some(&d) = chars.peek() {
                            if d.is_ascii_digit() {
                                chars.next();
                                d.to_digit(10).unwrap() as i32
                            } else if from_fd == 2 {
                                1
                            } else {
                                2
                            }
                        } else if from_fd == 2 {
                            1
                        } else {
                            2
                        };
                        tokens.push(Token::Redirect(RedirectKind::DupWrite(from_fd, target)));
                    } else {
                        tokens.push(Token::Redirect(if stderr {
                            RedirectKind::Stderr
                        } else {
                            RedirectKind::Stdout
                        }));
                    }
                    continue;
                }

                if saw_argument {
                    tokens.push(classify_word(std::mem::take(&mut current)));
                    saw_argument = false;
                }

                if chars.peek() == Some(&'>') {
                    chars.next();
                    tokens.push(Token::Redirect(RedirectKind::StdoutAppend));
                } else if chars.peek() == Some(&'&') {
                    chars.next();
                    if let Some(&d) = chars.peek() {
                        if d.is_ascii_digit() {
                            chars.next();
                            let target = d.to_digit(10).unwrap() as i32;
                            tokens.push(Token::Redirect(RedirectKind::DupWrite(1, target)));
                        } else {
                            tokens.push(Token::Redirect(RedirectKind::StdoutAndStderr));
                        }
                    } else {
                        tokens.push(Token::Redirect(RedirectKind::StdoutAndStderr));
                    }
                } else {
                    tokens.push(Token::Redirect(RedirectKind::Stdout));
                }
                continue;
            }
        }

        if c == '\'' || c == '"' {
            if c == '\'' {
                current.push(LITERAL_START);
            } else {
                current.push(DOUBLE_QUOTE_MARKER);
            }
            quote = Some(c);
            saw_argument = true;
            continue;
        }

        if c == '#' && !saw_argument {
            while let Some(&next) = chars.peek() {
                if next == '\n' {
                    break;
                }
                chars.next();
            }
            continue;
        }

        if c.is_whitespace() {
            if saw_argument {
                tokens.push(classify_word(std::mem::take(&mut current)));
                saw_argument = false;
            }
            continue;
        }

        current.push(c);
        saw_argument = true;
    }

    if escaped {
        return Err("unfinished escape at end of command".into());
    }

    if let Some(quote) = quote {
        return Err(format!("unterminated {} quote", quote));
    }

    if saw_argument {
        tokens.push(classify_word(current));
    }

    Ok(tokens)
}

fn classify_word(word: String) -> Token {
    match word.as_str() {
        "if" => Token::If,
        "then" => Token::Then,
        "else" => Token::Else,
        "fi" => Token::Fi,
        _ => Token::Word(word),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_command() {
        let command = parse_input("echo hello").unwrap().unwrap();

        assert_eq!(command.program, "echo");
        assert_eq!(command.args, vec!["hello"]);
    }

    #[test]
    fn ignores_unquoted_comments() {
        let command = parse_input("echo hello # trailing comment")
            .unwrap()
            .unwrap();

        assert_eq!(command.args, vec!["hello"]);
    }

    #[test]
    fn preserves_quoted_and_escaped_hashes() {
        let command = parse_input(r##"echo "#" \#value"##).unwrap().unwrap();

        assert_eq!(command.args, vec!["#", "#value"]);
    }

    #[test]
    fn parses_per_command_environment_assignments() {
        let pipeline = parse_pipeline("MODE=test printenv MODE").unwrap().unwrap();
        assert_eq!(pipeline.commands[0].program, "printenv");
        assert_eq!(pipeline.commands[0].args, vec!["MODE"]);
        assert_eq!(
            pipeline.commands[0].environment,
            vec![("MODE".into(), "test".into())]
        );
    }

    #[test]
    fn parses_assignment_as_export_command() {
        let pipeline = parse_pipeline("MODE=test").unwrap().unwrap();
        assert_eq!(pipeline.commands[0].program, "export");
        assert_eq!(pipeline.commands[0].args, vec!["MODE=test"]);
    }

    #[test]
    fn parses_quoted_argument() {
        let command = parse_input(r#"echo "hello world""#).unwrap().unwrap();

        assert_eq!(command.args, vec!["hello world"]);
    }

    #[test]
    fn parses_single_quotes() {
        let command = parse_input("echo 'hello world'").unwrap().unwrap();

        assert_eq!(command.args, vec!["hello world"]);
    }

    #[test]
    fn parses_escaped_space() {
        let command = parse_input(r"echo hello\ world").unwrap().unwrap();

        assert_eq!(command.args, vec!["hello world"]);
    }

    #[test]
    fn detects_unterminated_quote() {
        assert!(parse_input(r#"echo "hello"#).is_err());
    }

    #[test]
    fn detects_unfinished_escape() {
        assert!(parse_input(r"echo hello\").is_err());
    }

    #[test]
    fn handles_empty_input() {
        assert!(parse_input("   \t  ").unwrap().is_none());
    }

    #[test]
    fn preserves_empty_quoted_argument() {
        let command = parse_input(r#"echo "" test"#).unwrap().unwrap();

        assert_eq!(command.args, vec!["", "test"]);
    }

    #[test]
    fn expands_home_and_environment_variables() {
        let home = std::env::var("HOME").unwrap();
        let command = parse_input("echo $HOME ~/docs").unwrap().unwrap();

        assert_eq!(command.args, vec![home.clone(), format!("{home}/docs")]);
    }

    #[test]
    fn expands_previous_exit_status() {
        let pipeline = parse_pipeline_with_status("echo $?", 17).unwrap().unwrap();

        assert_eq!(pipeline.commands[0].args, vec!["17"]);
    }

    #[test]
    fn preserves_single_quoted_expansion_text() {
        let home = std::env::var("HOME").unwrap();
        let pipeline = parse_pipeline_with_status("echo '$HOME' '$?' '~' mixed$HOME", 17)
            .unwrap()
            .unwrap();

        assert_eq!(
            pipeline.commands[0].args,
            vec![
                "$HOME".to_string(),
                "$?".to_string(),
                "~".to_string(),
                format!("mixed{home}"),
            ]
        );
    }

    #[test]
    fn preserves_escaped_expansion_text() {
        let pipeline = parse_pipeline_with_status(r"echo \$HOME \$? \~", 17)
            .unwrap()
            .unwrap();

        assert_eq!(
            pipeline.commands[0].args,
            vec!["$HOME".to_string(), "$?".to_string(), "~".to_string()]
        );
    }

    #[test]
    fn does_not_expand_tilde_inside_double_quotes() {
        let home = std::env::var("HOME").unwrap();
        let command = parse_input(r#"echo "~" "~/docs" "$HOME""#)
            .unwrap()
            .unwrap();

        assert_eq!(
            command.args,
            vec!["~".to_string(), "~/docs".to_string(), home,]
        );
    }

    #[test]
    fn parses_pipeline() {
        let pipeline = parse_pipeline("printf hello | wc -c").unwrap().unwrap();

        assert_eq!(pipeline.commands.len(), 2);
        assert_eq!(pipeline.commands[0].program, "printf");
        assert_eq!(pipeline.commands[1].args, vec!["-c"]);
    }

    #[test]
    fn parses_background_marker() {
        let pipeline = parse_pipeline("sleep 1 &").unwrap().unwrap();

        assert!(pipeline.background);
        assert_eq!(pipeline.commands[0].program, "sleep");
    }

    #[test]
    fn parses_redirections() {
        let command = parse_input("cat < input > output >> log 2> errors 2>> errors.log")
            .unwrap()
            .unwrap();

        assert_eq!(
            command.redirects,
            vec![
                Redirection::Stdin("input".into()),
                Redirection::Stdout("output".into()),
                Redirection::StdoutAppend("log".into()),
                Redirection::Stderr("errors".into()),
                Redirection::StderrAppend("errors.log".into()),
            ]
        );
    }

    #[test]
    fn rejects_missing_redirection_target() {
        assert!(parse_input("echo hello >").is_err());
    }

    #[test]
    fn rejects_malformed_pipeline_operators() {
        for input in ["| echo", "echo |", "echo | | cat"] {
            assert!(
                parse_pipeline(input).is_err(),
                "expected rejection: {input}"
            );
        }
    }

    #[test]
    fn rejects_background_without_a_command() {
        assert!(parse_pipeline("&").is_err());
        assert!(parse_pipeline("echo | &").is_err());
    }

    #[test]
    fn expands_redirection_targets() {
        let home = std::env::var("HOME").unwrap();
        let command = parse_input(r#"echo hello > "$HOME/output" 2> ~/errors"#)
            .unwrap()
            .unwrap();

        assert_eq!(
            command.redirects,
            vec![
                Redirection::Stdout(format!("{home}/output")),
                Redirection::Stderr(format!("{home}/errors")),
            ]
        );
    }

    #[test]
    fn parses_if_statement() {
        let input = "if /bin/true then /bin/echo then_branch else /bin/echo else_branch fi";
        let ast = parse_ast(input, 0).unwrap().unwrap();

        match ast {
            Ast::If(if_stmt) => {
                assert_eq!(if_stmt.condition.commands[0].program, "/bin/true");
                assert_eq!(if_stmt.then_branch.len(), 1);
                assert_eq!(if_stmt.else_branch.as_ref().unwrap().len(), 1);
            }
            _ => panic!("expected If AST node"),
        }
    }

    #[test]
    fn parses_logical_and_and_or_sequence() {
        let ast = parse_ast("echo a && echo b || echo c", 0)
            .unwrap()
            .unwrap();
        match ast {
            Ast::Sequence(seq) => {
                assert_eq!(seq.items.len(), 3);
                assert!(seq.items[0].op.is_none());
                assert_eq!(seq.items[1].op, Some(LogicalOp::And));
                assert_eq!(seq.items[2].op, Some(LogicalOp::Or));
            }
            _ => panic!("expected Sequence AST node"),
        }
    }

    #[test]
    fn parses_semicolon_sequence() {
        let ast = parse_ast("echo a; echo b", 0).unwrap().unwrap();
        match ast {
            Ast::Sequence(seq) => {
                assert_eq!(seq.items.len(), 2);
                assert!(seq.items[0].op.is_none());
                assert!(seq.items[1].op.is_none());
            }
            _ => panic!("expected Sequence AST node"),
        }
    }

    #[test]
    fn evaluates_arithmetic_expansion() {
        assert_eq!(evaluate_arithmetic("2 + 3 * 4").unwrap(), 14);
        assert_eq!(evaluate_arithmetic("(10 - 2) / 4").unwrap(), 2);
        assert_eq!(evaluate_arithmetic("10 % 3").unwrap(), 1);
        assert_eq!(evaluate_arithmetic("5 > 3").unwrap(), 1);
        assert_eq!(evaluate_arithmetic("5 == 5").unwrap(), 1);
        assert!(evaluate_arithmetic("10 / 0").is_err());
    }

    #[test]
    fn expands_parameter_defaults() {
        unsafe { env::remove_var("MELCHIOR_UNSET_VAR") };
        let cmd = parse_input("echo ${MELCHIOR_UNSET_VAR:-default_val}")
            .unwrap()
            .unwrap();
        assert_eq!(cmd.args, vec!["default_val"]);

        unsafe { env::set_var("MELCHIOR_SET_VAR", "hello") };
        let cmd2 = parse_input("echo ${MELCHIOR_SET_VAR:-default_val} ${#MELCHIOR_SET_VAR}")
            .unwrap()
            .unwrap();
        assert_eq!(cmd2.args, vec!["hello", "5"]);
        unsafe { env::remove_var("MELCHIOR_SET_VAR") };
    }

    #[test]
    fn expands_glob_patterns() {
        let matched = expand_globs("Cargo.*");
        assert!(matched.contains(&"Cargo.toml".to_string()));
        assert!(matched.contains(&"Cargo.lock".to_string()));

        let unmatched = expand_globs("definitely_nonexistent_*.ext");
        assert_eq!(unmatched, vec!["definitely_nonexistent_*.ext"]);
    }

    #[test]
    fn parses_advanced_redirections() {
        let cmd = parse_input("ls 2>&1 &> all.log <<< 'hello'")
            .unwrap()
            .unwrap();
        assert_eq!(
            cmd.redirects,
            vec![
                Redirection::DupWrite(2, 1),
                Redirection::StdoutAndStderr("all.log".into()),
                Redirection::HereString("hello".into()),
            ]
        );
    }

    #[test]
    fn parses_structured_pipeline_with_projection() {
        let ast = parse_ast("docker ps --format json |> .ID", 0)
            .unwrap()
            .unwrap();

        match ast {
            Ast::StructuredPipeline(pipeline) => {
                assert_eq!(pipeline.stages, vec![StructuredOp::Project(".ID".into())]);
            }
            _ => panic!("expected StructuredPipeline"),
        }
    }

    #[test]
    fn parses_structured_pipeline_with_filter_and_projection() {
        let ast = parse_ast(
            "docker ps --format json |> filter (.status == \"running\") |> .ID",
            0,
        )
        .unwrap()
        .unwrap();

        match ast {
            Ast::StructuredPipeline(pipeline) => {
                assert_eq!(
                    pipeline.stages,
                    vec![
                        StructuredOp::Filter(FilterExpr {
                            field: ".status".into(),
                            op: FilterOp::Equal,
                            value: "running".into(),
                        }),
                        StructuredOp::Project(".ID".into()),
                    ]
                );
            }
            _ => panic!("expected StructuredPipeline"),
        }
    }

    #[test]
    fn parses_structured_pipeline_with_take_and_count() {
        let ast = parse_ast("cat items.json |> take 5 |> count", 0)
            .unwrap()
            .unwrap();

        match ast {
            Ast::StructuredPipeline(pipeline) => {
                assert_eq!(
                    pipeline.stages,
                    vec![StructuredOp::Take(5), StructuredOp::Count]
                );
            }
            _ => panic!("expected StructuredPipeline"),
        }
    }

    #[test]
    fn parses_structured_pipeline_with_formats() {
        let ast = parse_ast("curl https://api.example.com |> yaml", 0)
            .unwrap()
            .unwrap();

        match ast {
            Ast::StructuredPipeline(pipeline) => {
                assert_eq!(
                    pipeline.stages,
                    vec![StructuredOp::Format(OutputFormat::Yaml)]
                );
            }
            _ => panic!("expected StructuredPipeline"),
        }
    }

    #[test]
    fn parses_for_loop_syntax() {
        let ast = parse_ast("for item in one two three; do echo $item; done", 0)
            .unwrap()
            .unwrap();

        match ast {
            Ast::For(parsed_for) => {
                assert_eq!(parsed_for.variable, "item");
                assert_eq!(parsed_for.values, vec!["one", "two", "three"]);
                let body = parsed_for.parse_body(0).unwrap();
                assert_eq!(body.len(), 1);
            }
            _ => panic!("expected For loop Ast"),
        }
    }

    #[test]
    fn parses_while_loop_syntax() {
        let ast = parse_ast("while true; do echo running; done", 0)
            .unwrap()
            .unwrap();

        match ast {
            Ast::While(parsed_while) => {
                let cond = parsed_while.parse_condition(0).unwrap();
                let body = parsed_while.parse_body(0).unwrap();
                assert_eq!(cond.len(), 1);
                assert_eq!(body.len(), 1);
            }
            _ => panic!("expected While loop Ast"),
        }
    }

    #[test]
    fn preserves_non_escaped_characters_in_double_quotes() {
        let tokens = tokenize(r#"echo "\n" "\t" "\\""#).unwrap();
        match &tokens[1] {
            Token::Word(w) => assert_eq!(strip_markers(w), r#"\n"#),
            _ => panic!("expected Word token"),
        }
        match &tokens[2] {
            Token::Word(w) => assert_eq!(strip_markers(w), r#"\t"#),
            _ => panic!("expected Word token"),
        }
    }

    #[test]
    fn parses_function_definitions() {
        let ast1 = parse_ast("hello() { echo \"Hello world\"; }", 0).unwrap().unwrap();
        match ast1 {
            Ast::Function(func) => {
                assert_eq!(func.name, "hello");
                let body = func.parse_body(0).unwrap();
                assert_eq!(body.len(), 1);
            }
            _ => panic!("expected Function Ast"),
        }

        let ast2 = parse_ast("function greet() { echo $1; }", 0).unwrap().unwrap();
        match ast2 {
            Ast::Function(func) => {
                assert_eq!(func.name, "greet");
                assert_eq!(func.parse_body(0).unwrap().len(), 1);
            }
            _ => panic!("expected Function Ast"),
        }

        let ast3 = parse_ast("function my_cmd { echo first; echo second; }", 0).unwrap().unwrap();
        match ast3 {
            Ast::Function(func) => {
                assert_eq!(func.name, "my_cmd");
                let body = func.parse_body(0).unwrap();
                assert_eq!(body.len(), 2);
            }
            _ => panic!("expected Function Ast"),
        }
    }
}
