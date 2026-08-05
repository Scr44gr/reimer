use reimer_ast::{
    self as ast, Expression, GenericArgument, GenericParameter, Item, Pattern, Statement,
    TypeNameKind,
};

pub(crate) trait Visitor {
    fn item(&mut self, _item: &Item) {}
    fn function(&mut self, _function: &ast::Function) {}
    fn block(&mut self, _block: &ast::Block) {}
    fn statement(&mut self, _statement: &Statement) {}
    fn expression(&mut self, _expression: &Expression) {}
    fn pattern(&mut self, _pattern: &Pattern) {}
    fn type_name(&mut self, _type_name: &ast::TypeName) {}
}

pub(crate) fn program(visitor: &mut impl Visitor, program: &ast::Program) {
    for item in &program.items {
        visitor.item(item);
        match item {
            Item::Import(_) => {}
            Item::Function(function) => function_declaration(visitor, function),
            Item::ExternFunction(function) => {
                for parameter in &function.parameters {
                    type_name(visitor, &parameter.ty);
                }
                if let Some(return_type) = &function.return_type {
                    type_name(visitor, return_type);
                }
            }
            Item::Struct(declaration) => {
                generic_parameters(visitor, &declaration.generic_parameters);
                for field in &declaration.fields {
                    type_name(visitor, &field.ty);
                }
                where_predicates(visitor, &declaration.where_predicates);
            }
            Item::Enum(declaration) => {
                generic_parameters(visitor, &declaration.generic_parameters);
                for variant in &declaration.variants {
                    match &variant.payload {
                        ast::EnumVariantPayload::Unit => {}
                        ast::EnumVariantPayload::Tuple(types) => {
                            for ty in types {
                                type_name(visitor, ty);
                            }
                        }
                        ast::EnumVariantPayload::Struct(fields) => {
                            for field in fields {
                                type_name(visitor, &field.ty);
                            }
                        }
                    }
                }
                where_predicates(visitor, &declaration.where_predicates);
            }
            Item::TypeAlias(declaration) => {
                type_name(visitor, &declaration.target);
            }
            Item::Trait(declaration) => {
                generic_parameters(visitor, &declaration.generic_parameters);
                where_predicates(visitor, &declaration.where_predicates);
                for method in &declaration.methods {
                    generic_parameters(visitor, &method.generic_parameters);
                    for parameter in &method.parameters {
                        type_name(visitor, &parameter.ty);
                    }
                    if let Some(return_type) = &method.return_type {
                        type_name(visitor, return_type);
                    }
                    where_predicates(visitor, &method.where_predicates);
                }
            }
            Item::Impl(declaration) => {
                generic_parameters(visitor, &declaration.generic_parameters);
                if let Some(trait_type) = &declaration.trait_type {
                    type_name(visitor, trait_type);
                }
                type_name(visitor, &declaration.target);
                where_predicates(visitor, &declaration.where_predicates);
                for method in &declaration.methods {
                    function_declaration(visitor, method);
                }
            }
            Item::Constant(declaration) => {
                type_name(visitor, &declaration.ty);
                expression(visitor, &declaration.value);
            }
            Item::Static(declaration) => {
                type_name(visitor, &declaration.ty);
                expression(visitor, &declaration.value);
            }
            Item::Comptime(block) => self::block(visitor, &block.body),
        }
    }
}

pub(crate) fn function_declaration(visitor: &mut impl Visitor, function: &ast::Function) {
    visitor.function(function);
    generic_parameters(visitor, &function.generic_parameters);
    for parameter in &function.parameters {
        type_name(visitor, &parameter.ty);
    }
    if let Some(return_type) = &function.return_type {
        type_name(visitor, return_type);
    }
    where_predicates(visitor, &function.where_predicates);
    block(visitor, &function.body);
}

fn generic_parameters(visitor: &mut impl Visitor, parameters: &[GenericParameter]) {
    for parameter in parameters {
        match parameter {
            GenericParameter::Type { default, .. } => {
                if let Some(default) = default {
                    type_name(visitor, default);
                }
            }
            GenericParameter::Const { ty, default, .. } => {
                type_name(visitor, ty);
                if let Some(default) = default {
                    expression(visitor, default);
                }
            }
            GenericParameter::TypePack { .. } => {}
        }
    }
}

fn where_predicates(visitor: &mut impl Visitor, predicates: &[ast::WherePredicate]) {
    for predicate in predicates {
        type_name(visitor, &predicate.ty);
    }
}

pub(crate) fn block(visitor: &mut impl Visitor, block: &ast::Block) {
    visitor.block(block);
    for statement in &block.statements {
        visitor.statement(statement);
        match statement {
            Statement::Let(statement) => expression(visitor, &statement.initializer),
            Statement::Expression(statement) => expression(visitor, &statement.expression),
            Statement::Defer(statement) => expression(visitor, &statement.action),
            Statement::Return(statement) => {
                if let Some(value) = &statement.value {
                    expression(visitor, value);
                }
            }
            Statement::While(statement) => {
                expression(visitor, &statement.condition);
                self::block(visitor, &statement.body);
            }
            Statement::For(statement) => {
                pattern(visitor, &statement.pattern);
                expression(visitor, &statement.iterable);
                self::block(visitor, &statement.body);
            }
            Statement::Break(statement) => {
                if let Some(value) = &statement.value {
                    expression(visitor, value);
                }
            }
            Statement::Continue(_) => {}
        }
    }
    if let Some(tail) = &block.tail {
        expression(visitor, tail);
    }
}

pub(crate) fn expression(visitor: &mut impl Visitor, expression: &Expression) {
    visitor.expression(expression);
    match expression {
        Expression::Integer(_)
        | Expression::Float(_)
        | Expression::Character(_)
        | Expression::String(_)
        | Expression::CString(_)
        | Expression::Boolean(_)
        | Expression::Unit(_)
        | Expression::Path(_) => {}
        Expression::GenericFunction(function) => {
            for argument in &function.generic_arguments {
                generic_argument(visitor, argument);
            }
        }
        Expression::FormattedString(formatted) => {
            for fragment in &formatted.fragments {
                if let ast::FormattedStringFragment::Display(expression)
                | ast::FormattedStringFragment::Debug(expression) = fragment
                {
                    self::expression(visitor, expression);
                }
            }
        }
        Expression::Tuple(tuple) => {
            for element in &tuple.elements {
                self::expression(visitor, element);
            }
        }
        Expression::PackExpansion(expansion) => {
            self::expression(visitor, &expansion.template);
        }
        Expression::Array(array) => match &array.kind {
            reimer_ast::ArrayExpressionKind::List(elements) => {
                for element in elements {
                    self::expression(visitor, element);
                }
            }
            reimer_ast::ArrayExpressionKind::Repeat { value, length } => {
                self::expression(visitor, value);
                self::expression(visitor, length);
            }
        },
        Expression::Struct(structure) => {
            for field in &structure.fields {
                self::expression(visitor, &field.value);
            }
        }
        Expression::Unary(unary) => self::expression(visitor, &unary.operand),
        Expression::Binary(binary) => {
            self::expression(visitor, &binary.left);
            self::expression(visitor, &binary.right);
        }
        Expression::Call(call) => {
            self::expression(visitor, &call.callee);
            for argument in &call.generic_arguments {
                generic_argument(visitor, argument);
            }
            for argument in &call.arguments {
                self::expression(visitor, argument);
            }
        }
        Expression::If(conditional) => {
            self::expression(visitor, &conditional.condition);
            block(visitor, &conditional.then_branch);
            if let Some(else_branch) = &conditional.else_branch {
                self::expression(visitor, else_branch);
            }
        }
        Expression::Match(matching) => {
            self::expression(visitor, &matching.scrutinee);
            for arm in &matching.arms {
                pattern(visitor, &arm.pattern);
                if let Some(guard) = &arm.guard {
                    self::expression(visitor, guard);
                }
                self::expression(visitor, &arm.body);
            }
        }
        Expression::Loop(looping) => block(visitor, &looping.body),
        Expression::Unsafe(block) | Expression::Block(block) => self::block(visitor, block),
        Expression::Assignment(assignment) => {
            self::expression(visitor, &assignment.target);
            self::expression(visitor, &assignment.value);
        }
        Expression::Cast(cast) => {
            self::expression(visitor, &cast.value);
            type_name(visitor, &cast.target);
        }
        Expression::Field(field) => self::expression(visitor, &field.base),
        Expression::Index(index) => {
            self::expression(visitor, &index.base);
            for index in &index.indices {
                self::expression(visitor, index);
            }
        }
        Expression::Try { value, .. } => self::expression(visitor, value),
    }
}

fn generic_argument(visitor: &mut impl Visitor, argument: &GenericArgument) {
    match argument {
        GenericArgument::Type(ty) => type_name(visitor, ty),
        GenericArgument::Const(value) => self::expression(visitor, value),
        GenericArgument::Pack { template, .. } => {
            if let Some(template) = template {
                type_name(visitor, template);
            }
        }
    }
}

pub(crate) fn pattern(visitor: &mut impl Visitor, pattern: &Pattern) {
    visitor.pattern(pattern);
    match pattern {
        Pattern::Tuple { elements, .. } => {
            for element in elements {
                self::pattern(visitor, element);
            }
        }
        Pattern::EnumTuple { fields, .. } => {
            for field in fields {
                self::pattern(visitor, field);
            }
        }
        Pattern::EnumStruct { fields, .. } => {
            for field in fields {
                self::pattern(visitor, &field.pattern);
            }
        }
        Pattern::Wildcard(_)
        | Pattern::Identifier { .. }
        | Pattern::Integer { .. }
        | Pattern::Float { .. }
        | Pattern::Character(_)
        | Pattern::Boolean(_)
        | Pattern::Path(_) => {}
    }
}

pub(crate) fn type_name(visitor: &mut impl Visitor, type_name: &ast::TypeName) {
    visitor.type_name(type_name);
    match &type_name.kind {
        TypeNameKind::Path(_) | TypeNameKind::Unit => {}
        TypeNameKind::Function {
            parameters,
            return_type,
        } => {
            for parameter in parameters {
                self::type_name(visitor, parameter);
            }
            self::type_name(visitor, return_type);
        }
        TypeNameKind::Generic { arguments, .. } => {
            for argument in arguments {
                match argument {
                    GenericArgument::Type(ty) => self::type_name(visitor, ty),
                    GenericArgument::Const(value) => expression(visitor, value),
                    GenericArgument::Pack { template, .. } => {
                        if let Some(template) = template {
                            self::type_name(visitor, template);
                        }
                    }
                }
            }
        }
        TypeNameKind::PackExpansion { template, .. } => {
            if let Some(template) = template {
                self::type_name(visitor, template);
            }
        }
        TypeNameKind::Tuple(elements) => {
            for element in elements {
                self::type_name(visitor, element);
            }
        }
        TypeNameKind::Array { element, length } => {
            self::type_name(visitor, element);
            expression(visitor, length);
        }
        TypeNameKind::Slice(element) => self::type_name(visitor, element),
        TypeNameKind::Reference { target, .. } | TypeNameKind::RawPointer { target, .. } => {
            self::type_name(visitor, target);
        }
    }
}
