use super::*;

impl Engine {
    pub(super) fn frame_reply(&self, index: usize) -> Result<Reply> {
        let frame = self
            .session
            .stack
            .get(index)
            .cloned()
            .ok_or_else(|| anyhow!("no frame {}", index))?;
        Ok(Reply::Frame { index, frame })
    }

    pub(super) async fn cmd_print(&mut self, expression: String, context: &str) -> Result<Reply> {
        if !self.session.status.is_active() {
            bail!(NOT_RUNNING);
        }
        let frame_id = self.session.current_frame().map(|f| f.id.0);
        let resp = self
            .client()?
            .request(EvaluateArguments {
                expression: expression.clone(),
                frame_id,
                context: Some(context.into()),
            })
            .await;
        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                if let Some(eval) = self.array_length(&expression, frame_id).await {
                    return self.value_reply(eval).await;
                }
                let null = if context == "watch" {
                    self.null_prefix(&expression, frame_id).await
                } else {
                    None
                };
                return Err(match null {
                    Some(p) => anyhow!(
                        "cannot evaluate `{expression}`: `{p}` is null ({})",
                        e.to_string().trim()
                    ),
                    None => evaluate_error(&expression, e),
                });
            }
        };
        let eval = Evaluation::from(resp);
        if context == "repl" {
            // REPL evaluation may have side effects, so it is not repeated
            // to describe the value.
            self.invalidate_variables().await;
            return self.value_reply(eval).await;
        }
        self.eval_reply(&expression, eval).await
    }

    /// The longest member-access prefix of a failed expression that
    /// evaluates to null, e.g. `a.b` for `a.b.c` when `a.b` is null.
    /// Adapters often report such failures as a missing name instead.
    pub(super) async fn null_prefix(
        &self,
        expression: &str,
        frame_id: Option<i64>,
    ) -> Option<String> {
        let client = self.client().ok()?;
        for prefix in member_prefixes(expression).into_iter().rev() {
            let Ok(resp) = client
                .request(EvaluateArguments {
                    expression: prefix.to_owned(),
                    frame_id,
                    context: Some("watch".into()),
                })
                .await
            else {
                continue;
            };
            return matches!(resp.result.trim(), "null" | "None" | "nullptr" | "NULL")
                .then(|| prefix.to_owned());
        }
        None
    }

    /// `arr.Length` (or `LongLength`/`Count`) computed from the array's
    /// value, e.g. `{int[3]}`. netcoredbg cannot evaluate members of arrays.
    pub(super) async fn array_length(
        &self,
        expression: &str,
        frame_id: Option<i64>,
    ) -> Option<Evaluation> {
        let (receiver, member) = expression.trim().rsplit_once('.')?;
        let type_name = match member.trim() {
            "Length" | "Count" => "int",
            "LongLength" => "long",
            _ => return None,
        };
        let receiver = receiver.trim();
        if receiver.is_empty() {
            return None;
        }
        let resp = self
            .client()
            .ok()?
            .request(EvaluateArguments {
                expression: receiver.to_owned(),
                frame_id,
                context: Some("watch".into()),
            })
            .await
            .ok()?;
        let len = array_length_of(&resp.result)?;
        Some(Evaluation {
            value: len.to_string(),
            type_name: Some(type_name.into()),
            children: None,
        })
    }

    /// Build a value reply, replacing an opaque top-level value with its
    /// `ToString()` like `describe` does for children.
    pub(super) async fn eval_reply(
        &mut self,
        expression: &str,
        mut eval: Evaluation,
    ) -> Result<Reply> {
        let mut top = [Variable {
            name: expression.to_owned(),
            value: eval.value.clone(),
            type_name: eval.type_name.clone(),
            children: eval.children,
            evaluate_name: Some(expression.to_owned()),
        }];
        self.describe(&mut top).await;
        eval.value = std::mem::take(&mut top[0].value);
        self.value_reply(eval).await
    }

    pub(super) async fn value_reply(&mut self, eval: Evaluation) -> Result<Reply> {
        let mut children = match eval.children {
            Some(r) => self.variables(r).await?,
            None => Vec::new(),
        };
        self.describe(&mut children).await;
        Ok(Reply::Value(eval, children))
    }

    /// Replace values that only name their type (`{System.Guid}`) with
    /// the result of `ToString()`, when the adapter can evaluate it.
    pub(super) async fn describe(&mut self, vars: &mut [Variable]) {
        let frame_id = self.session.current_frame().map(|f| f.id.0);
        for v in vars.iter_mut().filter(|v| v.is_opaque()) {
            let Some(expr) = &v.evaluate_name else {
                continue;
            };
            let Ok(client) = self.client() else {
                return;
            };
            let Ok(resp) = client
                .request(EvaluateArguments {
                    expression: format!("({expr}).ToString()"),
                    frame_id,
                    context: Some("watch".into()),
                })
                .await
            else {
                continue;
            };
            let s = resp.result.trim();
            let s = s
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .unwrap_or(s);
            if !s.is_empty() && Some(s) != v.type_name.as_deref() && s != v.value.trim() {
                v.value = s.to_owned();
            }
        }
    }

    pub(super) async fn invalidate_variables(&mut self) {
        self.session.variable_cache.clear();
        self.emit(DebugEvent::VariablesChanged);
        self.refresh_watches().await;
    }

    /// Assign to an l-value: `setExpression` when supported, otherwise
    /// `setVariable` on the variable's container.
    pub(super) async fn cmd_set(&mut self, target: String, value: String) -> Result<Reply> {
        self.require_stopped()?;
        let target = target.trim().to_owned();
        if self.session.supports(Feature::SetExpression) {
            let eval = self.set_expression(target, value).await?;
            return self.value_reply(eval).await;
        }
        if !self.session.supports(Feature::SetVariable) {
            bail!("the debug adapter does not support assigning values");
        }
        let (container, name) = self.resolve_lvalue(&target).await?;
        let eval = self.set_variable(container, name, value).await?;
        self.value_reply(eval).await
    }

    pub(super) async fn set_expression(
        &mut self,
        expression: String,
        value: String,
    ) -> Result<Evaluation> {
        let frame_id = self.session.current_frame().map(|f| f.id.0);
        let resp = self
            .client()?
            .request(SetExpressionArguments {
                expression,
                value,
                frame_id,
            })
            .await?;
        self.invalidate_variables().await;
        Ok(Evaluation {
            value: resp.value,
            type_name: resp.type_.filter(|t| !t.is_empty()),
            children: VarRef::from_raw(resp.variables_reference),
        })
    }

    /// Find the container reference and child name for `target`.
    pub(super) async fn resolve_lvalue(&mut self, target: &str) -> Result<(VarRef, String)> {
        match split_lvalue(target) {
            None => {
                if self.session.scopes.is_empty()
                    && let Some(i) = self.session.selected_frame
                {
                    self.select_frame(i).await?;
                }
                let scopes = self.session.scopes.clone();
                for scope in scopes.iter().filter(|s| !s.expensive) {
                    if self
                        .variables(scope.reference)
                        .await?
                        .iter()
                        .any(|v| v.name == target)
                    {
                        return Ok((scope.reference, target.to_owned()));
                    }
                }
                bail!("no variable named `{target}` in the current frame")
            }
            Some((parent, child)) => {
                let frame_id = self.session.current_frame().map(|f| f.id.0);
                let resp = self
                    .client()?
                    .request(EvaluateArguments {
                        expression: parent.to_owned(),
                        frame_id,
                        context: Some("watch".into()),
                    })
                    .await?;
                let Some(r) = VarRef::from_raw(resp.variables_reference) else {
                    bail!("`{parent}` has no members");
                };
                let children = self.variables(r).await?;
                let bracketed = format!("[{child}]");
                let name = children
                    .iter()
                    .map(|v| &v.name)
                    .find(|n| *n == child || **n == bracketed)
                    .ok_or_else(|| anyhow!("`{parent}` has no member `{child}`"))?
                    .clone();
                Ok((r, name))
            }
        }
    }

    pub(super) async fn set_variable(
        &mut self,
        container: VarRef,
        name: String,
        value: String,
    ) -> Result<Evaluation> {
        if !self.session.supports(Feature::SetVariable) {
            if self.session.supports(Feature::SetExpression) {
                return self.set_expression(name, value).await;
            }
            bail!("the debug adapter does not support assigning values");
        }
        let resp = self
            .client()?
            .request(SetVariableArguments {
                variables_reference: container.0,
                name,
                value,
            })
            .await?;
        self.invalidate_variables().await;
        Ok(Evaluation {
            value: resp.value,
            type_name: resp.type_.filter(|t| !t.is_empty()),
            children: VarRef::from_raw(resp.variables_reference),
        })
    }

    pub(super) async fn cmd_complete(&mut self, text: String, column: usize) -> Result<Reply> {
        let column = column.min(text.chars().count());
        let token = token_start(&text, column);
        if !self.session.supports(Feature::Completions) {
            // Fall back to local variable names.
            if !self.session.status.is_stopped() {
                return Ok(Reply::Completions(Vec::new()));
            }
            let Reply::Locals(scopes) = self.cmd_locals().await? else {
                unreachable!()
            };
            let prefix: String = text.chars().skip(token).take(column - token).collect();
            let items = scopes
                .into_iter()
                .flat_map(|s| s.variables)
                .filter(|v| v.name.starts_with(&prefix))
                .map(|v| Completion {
                    label: v.name.clone(),
                    text: v.name,
                    kind: v.type_name,
                    start: token,
                    length: column - token,
                })
                .collect();
            return Ok(Reply::Completions(items));
        }
        if !self.session.status.is_active() {
            bail!(NOT_RUNNING);
        }
        let frame_id = self.session.current_frame().map(|f| f.id.0);
        let resp = self
            .client()?
            .request(CompletionsArguments {
                column: utf16_len(&text, column) as i64 + 1,
                text: text.clone(),
                frame_id,
            })
            .await?;
        let items = resp
            .targets
            .into_iter()
            .map(|t| {
                let (start, length) = match (t.start, t.length) {
                    (Some(s), l) => {
                        let s = char_offset(&text, (s - 1).max(0) as usize).min(column);
                        let l = l.map_or(0, |l| {
                            char_offset(&text, utf16_len(&text, s) + l.max(0) as usize) - s
                        });
                        (s, l)
                    }
                    (None, Some(l)) => {
                        let s = char_offset(
                            &text,
                            utf16_len(&text, column).saturating_sub(l.max(0) as usize),
                        );
                        (s, column - s)
                    }
                    (None, None) => (token, column - token),
                };
                Completion {
                    text: t.text.unwrap_or_else(|| t.label.clone()),
                    label: t.label,
                    kind: t.type_.or(t.detail),
                    start,
                    length,
                }
            })
            .collect();
        Ok(Reply::Completions(items))
    }

    pub(super) async fn cmd_locals(&mut self) -> Result<Reply> {
        self.require_stopped()?;
        if self.session.scopes.is_empty()
            && let Some(i) = self.session.selected_frame
        {
            self.select_frame(i).await?;
        }
        let mut scopes: Vec<_> = self
            .session
            .scopes
            .iter()
            .filter(|s| s.is_locals)
            .cloned()
            .collect();
        if scopes.is_empty() {
            scopes.extend(self.session.scopes.iter().find(|s| !s.expensive).cloned());
        }
        let mut out = Vec::new();
        for scope in scopes {
            let mut variables = self.variables(scope.reference).await?;
            self.describe(&mut variables).await;
            out.push(ScopeVariables {
                scope: scope.name,
                reference: scope.reference,
                variables,
            });
        }
        Ok(Reply::Locals(out))
    }

    // -----------------------------------------------------------------------
    // State refresh
    // -----------------------------------------------------------------------
    pub(super) async fn refresh_threads(&mut self) -> Result<()> {
        let resp = self.client()?.request(ThreadsArguments {}).await?;
        self.session.threads = resp.threads.into_iter().map(Into::into).collect();
        Ok(())
    }

    pub(super) async fn load_stack(&mut self, tid: ThreadId) -> Result<()> {
        let resp = self
            .client()?
            .request(StackTraceArguments {
                thread_id: tid.0,
                start_frame: None,
                levels: None,
            })
            .await?;
        self.session.on_resume();
        self.session.stack = resp.stack_frames.into_iter().map(Into::into).collect();
        if !self.session.stack.is_empty() {
            self.select_frame(0).await?;
        }
        Ok(())
    }

    pub(super) async fn select_frame(&mut self, index: usize) -> Result<()> {
        let frame = self
            .session
            .stack
            .get(index)
            .ok_or_else(|| anyhow!("no frame {}", index))?;
        let resp = self
            .client()?
            .request(ScopesArguments {
                frame_id: frame.id.0,
            })
            .await?;
        self.session.selected_frame = Some(index);
        self.session.scopes = resp.scopes.into_iter().map(Into::into).collect();
        Ok(())
    }

    pub(super) async fn variables(&mut self, r: VarRef) -> Result<Vec<Variable>> {
        if let Some(v) = self.session.variable_cache.get(&r) {
            return Ok(v.clone());
        }
        let resp = self
            .client()?
            .request(VariablesArguments {
                variables_reference: r.0,
            })
            .await?;
        let vars: Vec<Variable> = resp
            .variables
            .into_iter()
            .take(MAX_CHILDREN)
            .map(Into::into)
            .collect();
        self.session.variable_cache.insert(r, vars.clone());
        Ok(vars)
    }

    pub(super) async fn cmd_backtrace(&mut self) -> Result<Reply> {
        let tid = self.require_stopped()?;
        if self.session.stack.is_empty() {
            self.load_stack(tid).await?;
        }
        Ok(Reply::Backtrace {
            frames: self.session.stack.clone(),
            selected: self.session.selected_frame,
        })
    }

    pub(super) async fn cmd_threads(&mut self) -> Result<Reply> {
        if !self.session.status.is_active() {
            bail!(NOT_RUNNING);
        }
        self.refresh_threads().await?;
        Ok(Reply::Threads {
            threads: self.session.threads.clone(),
            selected: self.session.selected_thread,
        })
    }

    pub(super) async fn cmd_thread(&mut self, tid: ThreadId) -> Result<Reply> {
        self.require_stopped()?;
        if !self.session.threads.iter().any(|t| t.id == tid) {
            self.refresh_threads().await?;
        }
        if !self.session.threads.iter().any(|t| t.id == tid) {
            bail!("no thread {tid}");
        }
        self.session.selected_thread = Some(tid);
        self.load_stack(tid).await?;
        self.emit(DebugEvent::FrameChanged);
        self.refresh_watches().await;
        self.frame_reply(0)
    }

    pub(super) async fn cmd_frame(&mut self, sel: FrameSelector) -> Result<Reply> {
        let tid = self.require_stopped()?;
        if self.session.stack.is_empty() {
            self.load_stack(tid).await?;
        }
        let current = self.session.selected_frame.unwrap_or(0);
        let index = match sel {
            FrameSelector::Current => current,
            FrameSelector::Index(i) => i,
            FrameSelector::Up => current + 1,
            FrameSelector::Down => current
                .checked_sub(1)
                .ok_or_else(|| anyhow!("no frame {}", 0))?,
        };
        if index >= self.session.stack.len() {
            bail!("no frame {index}");
        }
        if index != current || self.session.selected_frame.is_none() {
            self.select_frame(index).await?;
            self.emit(DebugEvent::FrameChanged);
            self.refresh_watches().await;
        }
        self.frame_reply(index)
    }

    pub(super) async fn cmd_set_variable(
        &mut self,
        scope: VarRef,
        name: String,
        value: String,
    ) -> Result<Reply> {
        self.require_stopped()?;
        let eval = self.set_variable(scope, name, value).await?;
        self.value_reply(eval).await
    }
}

/// Split `a.b`, `a->b` or `a[2]` into parent and member; `None` for a
/// plain name. Only the outermost (last) access is split.
fn split_lvalue(target: &str) -> Option<(&str, &str)> {
    let target = target.trim();
    if let Some(inner) = target.strip_suffix(']') {
        let mut depth = 0;
        for (i, c) in inner.char_indices().rev() {
            match c {
                ']' => depth += 1,
                '[' if depth == 0 => {
                    let parent = inner[..i].trim();
                    return (!parent.is_empty()).then(|| (parent, inner[i + 1..].trim()));
                }
                '[' => depth -= 1,
                _ => {}
            }
        }
        return None;
    }
    let mut depth = 0;
    let bytes = target.as_bytes();
    for i in (0..bytes.len()).rev() {
        match bytes[i] {
            b')' | b']' => depth += 1,
            b'(' | b'[' => depth -= 1,
            b'.' if depth == 0 => {
                let parent = target[..i].strip_suffix('-').unwrap_or(&target[..i]);
                let parent = parent.trim();
                return (!parent.is_empty()).then(|| (parent, target[i + 1..].trim()));
            }
            b'>' if depth == 0 && i > 0 && bytes[i - 1] == b'-' => {
                let parent = target[..i - 1].trim();
                return (!parent.is_empty()).then(|| (parent, target[i + 1..].trim()));
            }
            _ => {}
        }
    }
    None
}

/// Char offset where the identifier ending at `column` starts.
fn token_start(text: &str, column: usize) -> usize {
    let chars: Vec<char> = text.chars().take(column).collect();
    let mut i = chars.len();
    while i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_' || chars[i - 1] == '$') {
        i -= 1;
    }
    i
}

/// UTF-16 length of the first `chars` chars of `text`.
fn utf16_len(text: &str, chars: usize) -> usize {
    text.chars().take(chars).map(char::len_utf16).sum()
}

/// Char offset of UTF-16 offset `units` in `text`.
fn char_offset(text: &str, units: usize) -> usize {
    let mut n = 0;
    for (i, c) in text.chars().enumerate() {
        if n >= units {
            return i;
        }
        n += c.len_utf16();
    }
    text.chars().count()
}

/// Element count from a .NET array value such as `{int[3]}` or
/// `{string[2, 4]}`.
fn array_length_of(value: &str) -> Option<u64> {
    let inner = value.trim().strip_prefix('{')?.strip_suffix('}')?;
    if !inner.ends_with(']') {
        return None;
    }
    inner.split('[').skip(1).find_map(|group| {
        let dims = group.strip_suffix(']')?;
        dims.split(',')
            .map(|d| d.trim().parse::<u64>().ok())
            .try_fold(1u64, |acc, d| acc.checked_mul(d?))
    })
}

#[cfg(test)]
mod array_length_tests {
    use super::array_length_of;
    use quickcheck_macros::quickcheck;

    fn array_value(dimensions: &[u64]) -> String {
        let dimensions = dimensions
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        format!("{{int[{dimensions}]}}")
    }

    #[quickcheck]
    fn array_dimensions_have_checked_product(first: u64, rest: Vec<u64>) -> bool {
        let dimensions: Vec<_> = std::iter::once(first).chain(rest).collect();
        let expected = dimensions
            .iter()
            .try_fold(1u64, |product, &n| product.checked_mul(n));
        array_length_of(&array_value(&dimensions)) == expected
    }

    #[quickcheck]
    fn zero_dimension_gives_zero(rest: Vec<u64>) -> bool {
        let dimensions: Vec<_> = std::iter::once(0).chain(rest).collect();
        array_length_of(&array_value(&dimensions)) == Some(0)
    }

    #[quickcheck]
    fn malformed_dimensions_are_rejected(number: u64) -> bool {
        array_length_of(&format!("{{int[{number}, invalid]}}")).is_none()
            && array_length_of(&format!("{{int[-{number}]}}")).is_none()
    }

    #[test]
    fn overflowing_dimensions_are_rejected() {
        assert_eq!(array_length_of("{int[18446744073709551615, 2]}"), None);
    }

    #[test]
    fn parses_array_values() {
        assert_eq!(array_length_of("{int[3]}"), Some(3));
        assert_eq!(array_length_of("{Foo.Bar[0]}"), Some(0));
        assert_eq!(array_length_of("{int[2, 4]}"), Some(8));
        assert_eq!(array_length_of("{int[2][]}"), Some(2));
        assert_eq!(array_length_of("{System.Guid}"), None);
        assert_eq!(array_length_of("{int[]}"), None);
        assert_eq!(array_length_of("3"), None);
    }
}

/// Prefixes of `expr` ending before each top-level `.` or `->`, shortest
/// first: `a[0].b.c` gives `a[0]` and `a[0].b`.
fn member_prefixes(expr: &str) -> Vec<&str> {
    let mut prefixes = Vec::new();
    let mut depth = 0i32;
    let mut quote = None;
    let bytes = expr.as_bytes();
    for (i, &c) in bytes.iter().enumerate() {
        match (quote, c) {
            (Some(q), _) if c == q && bytes.get(i.wrapping_sub(1)) != Some(&b'\\') => quote = None,
            (Some(_), _) => {}
            (None, b'"' | b'\'') => quote = Some(c),
            (None, b'(' | b'[' | b'{') => depth += 1,
            (None, b')' | b']' | b'}') => depth -= 1,
            (None, b'.') if depth == 0 && i > 0 => prefixes.push(&expr[..i]),
            (None, b'-') if depth == 0 && i > 0 && bytes.get(i + 1) == Some(&b'>') => {
                prefixes.push(&expr[..i])
            }
            _ => {}
        }
    }
    prefixes.retain(|p| !p.trim().is_empty() && !p.trim().bytes().all(|c| c.is_ascii_digit()));
    prefixes
}

/// Name the failing expression and explain bare HRESULTs from netcoredbg.
pub(super) fn evaluate_error(expression: &str, e: anyhow::Error) -> anyhow::Error {
    let message = e.to_string();
    let hint = [
        (
            "0x80070057",
            "invalid argument; often a member access on a null value or an expression \
             the evaluator does not support",
        ),
        ("0x80004005", "unspecified failure"),
    ]
    .into_iter()
    .find(|(code, _)| message.to_ascii_lowercase().contains(code))
    .map(|(_, hint)| format!(" ({hint})"))
    .unwrap_or_default();
    anyhow!("cannot evaluate `{expression}`: {message}{hint}")
}

#[cfg(test)]
mod evaluate_error_tests {
    use super::*;
    use quickcheck_macros::quickcheck;

    #[quickcheck]
    fn member_chains_split_at_top_level(accesses: Vec<(u8, u16)>) -> bool {
        let mut expression = String::from("root");
        let mut expected_prefixes = Vec::new();
        let mut expected_lvalue = None;
        for (kind, number) in accesses {
            let parent = expression.clone();
            let member = match kind % 4 {
                0 | 1 => {
                    expected_prefixes.push(parent.clone());
                    let member = format!("member{number}");
                    expression.push_str(if kind % 4 == 0 { "." } else { "->" });
                    expression.push_str(&member);
                    member
                }
                2 => {
                    let index = format!("indices[{number}]");
                    expression.push_str(&format!("[{index}]"));
                    index
                }
                _ => {
                    // Member access inside a call must not create a prefix.
                    let index = format!("lookup(item.member{number})");
                    expression.push_str(&format!("[{index}]"));
                    index
                }
            };
            expected_lvalue = Some((parent, member));
        }
        let actual_prefixes: Vec<_> = member_prefixes(&expression)
            .into_iter()
            .map(str::to_owned)
            .collect();
        let actual_lvalue = split_lvalue(&expression)
            .map(|(parent, member)| (parent.to_owned(), member.to_owned()));
        actual_prefixes == expected_prefixes && actual_lvalue == expected_lvalue
    }

    #[quickcheck]
    fn quoted_member_access_is_not_a_prefix(number: u16, arrow: bool) -> bool {
        let parent = format!("root[\"key.{number}->value\"]");
        let expression = format!("{parent}{}member", if arrow { "->" } else { "." });
        member_prefixes(&expression) == vec![parent.as_str()]
    }

    #[test]
    fn names_expression_and_explains_hresult() {
        let e = evaluate_error("a.b", anyhow!("evaluate failed: error 0x80070057"));
        let s = e.to_string();
        assert!(
            s.starts_with("cannot evaluate `a.b`: evaluate failed"),
            "{s}"
        );
        assert!(s.contains("null"), "{s}");
    }

    #[test]
    fn splits_member_prefixes() {
        assert_eq!(
            member_prefixes("Issue[0].Diagnostics.Length"),
            ["Issue[0]", "Issue[0].Diagnostics"]
        );
        assert_eq!(member_prefixes("f(a.b).c"), ["f(a.b)"]);
        assert_eq!(member_prefixes("p->next->x"), ["p", "p->next"]);
        assert_eq!(member_prefixes("s[\"a.b\"].x"), ["s[\"a.b\"]"]);
        assert!(member_prefixes("x").is_empty());
    }
}

#[cfg(test)]
mod lvalue_tests {
    use super::*;
    use quickcheck_macros::quickcheck;

    #[quickcheck]
    fn character_offsets_roundtrip_through_utf16(text: String, offset: usize) -> bool {
        let offset = offset % (text.chars().count() + 1);
        char_offset(&text, utf16_len(&text, offset)) == offset
    }

    #[quickcheck]
    fn utf16_offsets_round_up_to_character_boundaries(text: String, units: usize) -> bool {
        let total = text.encode_utf16().count();
        [units, units % (total + 2)].into_iter().all(|units| {
            let offset = char_offset(&text, units);
            let boundary = utf16_len(&text, offset);
            offset <= text.chars().count()
                && boundary >= units.min(total)
                && (offset == 0 || utf16_len(&text, offset - 1) < units.min(total))
        })
    }

    #[quickcheck]
    fn token_start_finds_trailing_identifier(text: String, column: usize) -> bool {
        let column = column % (text.chars().count() + 2);
        let prefix: Vec<_> = text.chars().take(column).collect();
        let is_identifier = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
        let trailing = prefix
            .iter()
            .rev()
            .take_while(|&&c| is_identifier(c))
            .count();
        token_start(&text, column) == prefix.len() - trailing
    }

    #[test]
    fn splits_lvalues() {
        assert_eq!(split_lvalue("x"), None);
        assert_eq!(split_lvalue("p.x"), Some(("p", "x")));
        assert_eq!(split_lvalue("a.b.c"), Some(("a.b", "c")));
        assert_eq!(split_lvalue("p->x"), Some(("p", "x")));
        assert_eq!(split_lvalue("v[2]"), Some(("v", "2")));
        assert_eq!(split_lvalue("m[a[1]]"), Some(("m", "a[1]")));
        assert_eq!(split_lvalue("f(a.b)"), None);
    }

    #[test]
    fn completion_offsets() {
        assert_eq!(token_start("p.po", 4), 2);
        assert_eq!(token_start("x + ab", 6), 4);
        assert_eq!(utf16_len("é😀a", 3), 4);
        assert_eq!(char_offset("é😀a", 3), 2);
    }
}
