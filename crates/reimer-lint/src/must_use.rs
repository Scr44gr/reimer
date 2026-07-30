use std::collections::{HashMap, HashSet};

use reimer_hir::{self as hir, Expression, ExpressionKind, FunctionId, Statement};
use reimer_types::Type;

use crate::{Finding, Severity};

pub(crate) fn lint(program: &hir::Program) -> Vec<Finding> {
    let required_functions = program
        .functions
        .iter()
        .filter(|function| function.attributes.must_use)
        .map(|function| function.id)
        .collect::<HashSet<_>>();
    let function_names = program
        .functions
        .iter()
        .map(|function| (function.id, function.name.as_str()))
        .collect::<HashMap<_, _>>();
    let mut findings = Vec::new();
    for function in &program.functions {
        lint_block(
            &function.body,
            program,
            &required_functions,
            &function_names,
            &mut findings,
        );
    }
    findings
}

fn lint_block(
    block: &hir::Block,
    program: &hir::Program,
    required_functions: &HashSet<FunctionId>,
    function_names: &HashMap<FunctionId, &str>,
    findings: &mut Vec<Finding>,
) {
    for statement in &block.statements {
        match statement {
            Statement::Expression(expression) => {
                if let Some(reason) =
                    discarded_reason(expression, program, required_functions, function_names)
                {
                    findings.push(Finding {
                        code: "L2020".to_owned(),
                        severity: Severity::Warning,
                        message: format!("unused value that {reason}"),
                        span: expression.span,
                        help: Some(
                            "bind, return, or explicitly consume the value instead of discarding it"
                                .to_owned(),
                        ),
                        fixes: Vec::new(),
                    });
                }
                lint_expression(
                    expression,
                    program,
                    required_functions,
                    function_names,
                    findings,
                );
            }
            Statement::Let { initializer, .. } => lint_expression(
                initializer,
                program,
                required_functions,
                function_names,
                findings,
            ),
            Statement::Defer { action, .. } => lint_expression(
                action,
                program,
                required_functions,
                function_names,
                findings,
            ),
            Statement::Return { value, .. } | Statement::Break { value, .. } => {
                if let Some(value) = value {
                    lint_expression(value, program, required_functions, function_names, findings);
                }
            }
            Statement::While {
                condition, body, ..
            } => {
                lint_expression(
                    condition,
                    program,
                    required_functions,
                    function_names,
                    findings,
                );
                lint_block(body, program, required_functions, function_names, findings);
            }
            Statement::For { iterable, body, .. } => {
                lint_expression(
                    iterable,
                    program,
                    required_functions,
                    function_names,
                    findings,
                );
                lint_block(body, program, required_functions, function_names, findings);
            }
            Statement::Continue(_) => {}
        }
    }
    if let Some(tail) = &block.tail {
        lint_expression(tail, program, required_functions, function_names, findings);
    }
}

fn lint_expression(
    expression: &Expression,
    program: &hir::Program,
    required_functions: &HashSet<FunctionId>,
    function_names: &HashMap<FunctionId, &str>,
    findings: &mut Vec<Finding>,
) {
    match &expression.kind {
        ExpressionKind::If(conditional) => {
            lint_expression(
                &conditional.condition,
                program,
                required_functions,
                function_names,
                findings,
            );
            lint_block(
                &conditional.then_branch,
                program,
                required_functions,
                function_names,
                findings,
            );
            if let Some(alternative) = &conditional.else_branch {
                lint_expression(
                    alternative,
                    program,
                    required_functions,
                    function_names,
                    findings,
                );
            }
        }
        ExpressionKind::Match(matching) => {
            lint_expression(
                &matching.scrutinee,
                program,
                required_functions,
                function_names,
                findings,
            );
            for arm in &matching.arms {
                if let Some(guard) = &arm.guard {
                    lint_expression(guard, program, required_functions, function_names, findings);
                }
                lint_expression(
                    &arm.body,
                    program,
                    required_functions,
                    function_names,
                    findings,
                );
            }
        }
        ExpressionKind::Loop(looping) => lint_block(
            &looping.body,
            program,
            required_functions,
            function_names,
            findings,
        ),
        ExpressionKind::Block(block) => {
            lint_block(block, program, required_functions, function_names, findings);
        }
        _ => {}
    }
}

fn discarded_reason<'program>(
    expression: &Expression,
    program: &'program hir::Program,
    required_functions: &HashSet<FunctionId>,
    function_names: &HashMap<FunctionId, &'program str>,
) -> Option<String> {
    if let ExpressionKind::Call { function, .. } = expression.kind
        && required_functions.contains(&function)
    {
        let name = function_names.get(&function).copied().unwrap_or("function");
        return Some(format!("`{name}` marked with `@must_use` returned"));
    }
    type_requires_use(program, expression.ty).then(|| {
        format!(
            "has type `{}` marked with `@must_use`",
            type_name(program, expression.ty)
        )
    })
}

fn type_requires_use(program: &hir::Program, ty: Type) -> bool {
    type_definition(program, ty).is_some_and(|definition| definition.must_use)
}

fn type_name(program: &hir::Program, ty: Type) -> String {
    type_definition(program, ty)
        .and_then(|definition| definition.name.clone())
        .unwrap_or_else(|| ty.to_string())
}

fn type_definition(program: &hir::Program, ty: Type) -> Option<&hir::TypeDefinition> {
    let (Type::Struct(id)
    | Type::Enum(id)
    | Type::Tuple(id)
    | Type::Array(id)
    | Type::Reference(id)
    | Type::RawPointer(id)
    | Type::Slice(id)
    | Type::Function(id)) = ty
    else {
        return None;
    };
    program.types.get(usize::try_from(id.0).ok()?)
}
