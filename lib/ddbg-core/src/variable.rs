use ddbg_dap::protocol as dap;

/// DAP `variablesReference`. Never survives a resume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VarRef(pub i64);

impl VarRef {
    pub(crate) fn from_raw(r: i64) -> Option<Self> {
        (r > 0).then_some(Self(r))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub name: String,
    pub reference: VarRef,
    pub expensive: bool,
    /// The adapter marked this scope as local variables.
    pub is_locals: bool,
}

impl From<dap::Scope> for Scope {
    fn from(s: dap::Scope) -> Self {
        let is_locals = s.presentation_hint.as_deref() == Some("locals")
            || s.name.eq_ignore_ascii_case("locals");
        Self {
            name: s.name,
            reference: VarRef(s.variables_reference),
            expensive: s.expensive,
            is_locals,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variable {
    pub name: String,
    pub value: String,
    pub type_name: Option<String>,
    /// Set when the variable has children.
    pub children: Option<VarRef>,
}

impl From<dap::Variable> for Variable {
    fn from(v: dap::Variable) -> Self {
        Self {
            name: v.name,
            value: v.value,
            type_name: v.type_.filter(|t| !t.is_empty()),
            children: VarRef::from_raw(v.variables_reference),
        }
    }
}

/// Result of evaluating an expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evaluation {
    pub value: String,
    pub type_name: Option<String>,
    pub children: Option<VarRef>,
}

impl From<dap::EvaluateResponse> for Evaluation {
    fn from(e: dap::EvaluateResponse) -> Self {
        Self {
            value: e.result,
            type_name: e.type_.filter(|t| !t.is_empty()),
            children: VarRef::from_raw(e.variables_reference),
        }
    }
}
