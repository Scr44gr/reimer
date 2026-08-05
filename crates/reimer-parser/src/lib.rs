//! Recursive-descent parsing for Reimer source files.

use std::mem::discriminant;

use reimer_ast::{
    ArrayExpression, ArrayExpressionKind, AssignmentExpression, AssignmentOperator, Attribute,
    AttributeArgument, BinaryExpression, BinaryOperator, Block, BooleanLiteral, BreakStatement,
    CallExpression, CastExpression, CharacterLiteral, ComptimeBlock, ConstantDeclaration,
    DeferStatement, EnumDeclaration, EnumVariant, EnumVariantPayload, Expression,
    ExpressionStatement, ExternFunction, FieldExpression, FieldInitializer, FieldName,
    FloatLiteral, ForStatement, FormattedStringExpression, FormattedStringFragment, Function,
    GenericArgument, GenericFunctionExpression, GenericParameter, Identifier, IfExpression,
    ImplDeclaration, ImportDeclaration, ImportKind, ImportedName, IndexExpression, IntegerLiteral,
    Item, LetStatement, LoopExpression, MatchArm, MatchExpression, PackExpansionExpression,
    Parameter, Path, Pattern, PatternField, Program, ReturnStatement, Statement, StaticDeclaration,
    StringLiteral, StructDeclaration, StructExpression, StructField, TraitDeclaration, TraitMethod,
    TupleExpression, TypeAliasDeclaration, TypeName, TypeNameKind, UnaryExpression, UnaryOperator,
    WherePredicate, WhileStatement,
};
use reimer_diagnostics::{Diagnostic, Span};
use reimer_lexer::{
    FormattedStringFragment as LexicalFormattedFragment, FormattingStyle, Token, TokenKind,
};

const MAX_NESTING_DEPTH: usize = 64;

/// Parses a token stream into a Reimer syntax tree.
///
/// # Errors
///
/// Returns accumulated syntax diagnostics after recovering at statement and
/// top-level declaration boundaries.
pub fn parse(tokens: &[Token]) -> Result<Program, Vec<Diagnostic>> {
    Parser::new(tokens).parse_program()
}

struct Parser<'tokens> {
    tokens: &'tokens [Token],
    cursor: usize,
    diagnostics: Vec<Diagnostic>,
    allow_struct_expression: bool,
    pending_type_greater: usize,
    nesting_depth: usize,
}

impl<'tokens> Parser<'tokens> {
    fn new(tokens: &'tokens [Token]) -> Self {
        Self {
            tokens,
            cursor: 0,
            diagnostics: Vec::new(),
            allow_struct_expression: true,
            pending_type_greater: 0,
            nesting_depth: 0,
        }
    }

    fn parse_program(mut self) -> Result<Program, Vec<Diagnostic>> {
        let mut items = Vec::new();

        while !self.at(&TokenKind::Eof) {
            let item_start = self.cursor;
            if let Some(parsed) = self.parse_item() {
                items.extend(parsed);
            } else {
                if self.cursor == item_start {
                    self.advance();
                }
                self.synchronize_top_level();
            }
        }

        if self.diagnostics.is_empty() {
            Ok(Program { items })
        } else {
            Err(self.diagnostics)
        }
    }

    fn parse_complete_expression(mut self) -> Result<Expression, Vec<Diagnostic>> {
        let expression = self.parse_expression();
        if expression.is_some() && !self.at(&TokenKind::Eof) {
            self.diagnostics.push(
                Diagnostic::error(
                    "E1021",
                    "unexpected tokens after formatted string expression",
                    self.current().span,
                )
                .with_help("keep one complete expression inside each placeholder"),
            );
        }
        match (expression, self.diagnostics.is_empty()) {
            (Some(expression), true) => Ok(expression),
            _ => Err(self.diagnostics),
        }
    }

    fn parse_item(&mut self) -> Option<Vec<Item>> {
        let attributes = self.parse_attributes()?;
        let has_visibility = self.at(&TokenKind::Pub);
        let declaration_offset = usize::from(has_visibility);
        let declaration = &self.token_at(declaration_offset).kind;

        if matches!(declaration, TokenKind::Comptime)
            && self.at_offset(declaration_offset + 1, &TokenKind::LeftBrace)
        {
            return self.parse_comptime_item(&attributes, has_visibility);
        }
        if self.at(&TokenKind::Extern)
            || (self.at(&TokenKind::Pub) && self.at_offset(1, &TokenKind::Extern))
        {
            return self.parse_extern_declarations(&attributes);
        }
        if self.at(&TokenKind::Fn)
            || (self.at(&TokenKind::Pub) && self.at_offset(1, &TokenKind::Fn))
            || (self.at(&TokenKind::Comptime) && self.at_offset(1, &TokenKind::Fn))
            || (self.at(&TokenKind::Pub)
                && self.at_offset(1, &TokenKind::Comptime)
                && self.at_offset(2, &TokenKind::Fn))
        {
            return self
                .parse_function(attributes)
                .map(|function| vec![Item::Function(function)]);
        }
        if self.at_optional_public(&TokenKind::Const) {
            return self.parse_constant_item(&attributes);
        }
        if self.at_optional_public(&TokenKind::Static) {
            return self.parse_static_item(&attributes);
        }
        if self.at(&TokenKind::Impl) {
            if !attributes.is_empty() {
                return self.unsupported_attribute_target(&attributes, "implementation block");
            }
            return self
                .parse_impl_declaration()
                .map(|declaration| vec![Item::Impl(declaration)]);
        }

        if self.at(&TokenKind::Trait)
            || (self.at(&TokenKind::Pub) && self.at_offset(1, &TokenKind::Trait))
        {
            if !attributes.is_empty() {
                return self.unsupported_attribute_target(&attributes, "trait declaration");
            }
            return self
                .parse_trait_declaration()
                .map(|declaration| vec![Item::Trait(declaration)]);
        }

        if self.at(&TokenKind::Struct)
            || (self.at(&TokenKind::Pub) && self.at_offset(1, &TokenKind::Struct))
        {
            return self
                .parse_struct_declaration(attributes)
                .map(|declaration| vec![Item::Struct(declaration)]);
        }

        if self.at(&TokenKind::Enum)
            || (self.at(&TokenKind::Pub) && self.at_offset(1, &TokenKind::Enum))
        {
            return self
                .parse_enum_declaration(attributes)
                .map(|declaration| vec![Item::Enum(declaration)]);
        }

        if self.at_optional_public(&TokenKind::Type) {
            if !attributes.is_empty() {
                return self.unsupported_attribute_target(&attributes, "type alias");
            }
            return self
                .parse_type_alias()
                .map(|declaration| vec![Item::TypeAlias(declaration)]);
        }

        if self.at(&TokenKind::From)
            || self.at(&TokenKind::Import)
            || (self.at(&TokenKind::Pub)
                && matches!(&self.token_at(1).kind, TokenKind::From | TokenKind::Import))
        {
            if !attributes.is_empty() {
                return self.unsupported_attribute_target(&attributes, "import declaration");
            }
            return self
                .parse_import()
                .map(|declaration| vec![Item::Import(declaration)]);
        }

        let token = self.current();
        self.diagnostics.push(
            Diagnostic::error("E1001", "expected a declaration", token.span).with_help(
                "start with `fn`, `const`, `static`, `comptime`, `struct`, `enum`, `type`, `trait`, `impl`, `extern`, or an import",
            ),
        );
        None
    }

    fn at_optional_public(&self, declaration: &TokenKind) -> bool {
        self.at(declaration) || (self.at(&TokenKind::Pub) && self.at_offset(1, declaration))
    }

    fn parse_constant_item(&mut self, attributes: &[Attribute]) -> Option<Vec<Item>> {
        if !attributes.is_empty() {
            self.diagnostics.push(
                Diagnostic::error(
                    "E1020",
                    "M10 attributes do not apply to constants",
                    attributes[0].span,
                )
                .with_help("remove the attribute from the constant declaration"),
            );
            return None;
        }
        self.parse_constant_declaration()
            .map(|constant| vec![Item::Constant(constant)])
    }

    fn parse_static_item(&mut self, attributes: &[Attribute]) -> Option<Vec<Item>> {
        if !attributes.is_empty() {
            self.diagnostics.push(
                Diagnostic::error(
                    "E1020",
                    "attributes do not apply to statics",
                    attributes[0].span,
                )
                .with_help("remove the attribute from the static declaration"),
            );
            return None;
        }
        self.parse_static_declaration()
            .map(|declaration| vec![Item::Static(declaration)])
    }

    fn parse_comptime_item(
        &mut self,
        attributes: &[Attribute],
        has_visibility: bool,
    ) -> Option<Vec<Item>> {
        if has_visibility {
            self.diagnostics.push(
                Diagnostic::error(
                    "E1020",
                    "an unnamed `comptime` block cannot be public",
                    self.current().span,
                )
                .with_help("remove `pub` from the block"),
            );
            return None;
        }
        if !attributes.is_empty() {
            self.diagnostics.push(
                Diagnostic::error(
                    "E1020",
                    "attributes cannot be attached to an unnamed `comptime` block",
                    attributes[0].span,
                )
                .with_help("attach the attribute to a function or type declaration"),
            );
            return None;
        }
        self.parse_comptime_block()
            .map(|block| vec![Item::Comptime(block)])
    }

    fn parse_attributes(&mut self) -> Option<Vec<Attribute>> {
        let mut attributes = Vec::new();
        while let Some(at) = self.take(&TokenKind::At) {
            let name = self.expect_identifier("attribute name")?;
            let mut arguments = Vec::new();
            let end = if self.take(&TokenKind::LeftParen).is_some() {
                if !self.at(&TokenKind::RightParen) {
                    loop {
                        arguments.push(self.parse_attribute_argument()?);
                        if self.take(&TokenKind::Comma).is_none() || self.at(&TokenKind::RightParen)
                        {
                            break;
                        }
                    }
                }
                self.expect_symbol(&TokenKind::RightParen, "`)` after attribute arguments")?
                    .span
                    .end
            } else {
                name.span.end
            };
            attributes.push(Attribute {
                name,
                arguments,
                span: Span::new(at.span.start, end),
            });
        }
        Some(attributes)
    }

    fn parse_attribute_argument(&mut self) -> Option<AttributeArgument> {
        let token = self.advance();
        match token.kind {
            TokenKind::Identifier(name) => Some(AttributeArgument::Identifier(Identifier {
                name,
                span: token.span,
            })),
            TokenKind::Integer(spelling) => {
                let Expression::Integer(literal) =
                    self.parse_integer_literal(&spelling, token.span)?
                else {
                    return None;
                };
                Some(AttributeArgument::Integer(literal))
            }
            TokenKind::String(value) => Some(AttributeArgument::String(StringLiteral {
                value,
                span: token.span,
            })),
            _ => {
                self.diagnostics.push(
                    Diagnostic::error("E1020", "invalid attribute argument", token.span)
                        .with_help("use an identifier, integer, or string literal"),
                );
                None
            }
        }
    }

    fn unsupported_attribute_target(
        &mut self,
        attributes: &[Attribute],
        target: &str,
    ) -> Option<Vec<Item>> {
        self.diagnostics.push(
            Diagnostic::error(
                "E1020",
                format!("attributes are not supported on this {target}"),
                attributes[0].span,
            )
            .with_help("move the attribute to a function, struct, or enum"),
        );
        None
    }

    fn parse_struct_declaration(
        &mut self,
        attributes: Vec<Attribute>,
    ) -> Option<StructDeclaration> {
        let start = attributes
            .first()
            .map_or(self.current().span.start, |attribute| attribute.span.start);
        let is_public = self.take(&TokenKind::Pub).is_some();
        self.expect_symbol(&TokenKind::Struct, "`struct`")?;
        let name = self.expect_identifier("struct name")?;
        let generic_parameters = self.parse_generic_parameters()?;
        let where_predicates = self.parse_where_clause()?;
        self.expect_symbol(&TokenKind::LeftBrace, "`{` before struct fields")?;
        let fields = self.parse_struct_fields()?;
        let end = self.expect_symbol(&TokenKind::RightBrace, "`}` after struct fields")?;
        Some(StructDeclaration {
            attributes,
            is_public,
            name,
            generic_parameters,
            fields,
            where_predicates,
            span: Span::new(start, end.span.end),
        })
    }

    fn parse_struct_fields(&mut self) -> Option<Vec<StructField>> {
        let mut fields = Vec::new();
        while !self.at(&TokenKind::RightBrace) && !self.at(&TokenKind::Eof) {
            let start = self.current().span.start;
            let is_public = self.take(&TokenKind::Pub).is_some();
            let name = self.expect_identifier("field name")?;
            self.expect_symbol(&TokenKind::Colon, "`:` after the field name")?;
            let ty = self.parse_type_name()?;
            let span = Span::new(start, ty.span.end);
            fields.push(StructField {
                is_public,
                name,
                ty,
                span,
            });
            if self.take(&TokenKind::Comma).is_none() {
                break;
            }
        }
        Some(fields)
    }

    fn parse_enum_declaration(&mut self, attributes: Vec<Attribute>) -> Option<EnumDeclaration> {
        let start = attributes
            .first()
            .map_or(self.current().span.start, |attribute| attribute.span.start);
        let is_public = self.take(&TokenKind::Pub).is_some();
        self.expect_symbol(&TokenKind::Enum, "`enum`")?;
        let name = self.expect_identifier("enum name")?;
        let generic_parameters = self.parse_generic_parameters()?;
        let where_predicates = self.parse_where_clause()?;
        self.expect_symbol(&TokenKind::LeftBrace, "`{` before enum variants")?;
        let mut variants = Vec::new();
        while !self.at(&TokenKind::RightBrace) && !self.at(&TokenKind::Eof) {
            variants.push(self.parse_enum_variant()?);
            if self.take(&TokenKind::Comma).is_none() {
                break;
            }
        }
        let end = self.expect_symbol(&TokenKind::RightBrace, "`}` after enum variants")?;
        Some(EnumDeclaration {
            attributes,
            is_public,
            name,
            generic_parameters,
            variants,
            where_predicates,
            span: Span::new(start, end.span.end),
        })
    }

    fn parse_enum_variant(&mut self) -> Option<EnumVariant> {
        let name = self.expect_identifier("enum variant name")?;
        let start = name.span.start;
        let (payload, end) = if self.take(&TokenKind::LeftParen).is_some() {
            let types = self.parse_type_list(&TokenKind::RightParen)?;
            let end = self.expect_symbol(&TokenKind::RightParen, "`)` after variant fields")?;
            (EnumVariantPayload::Tuple(types), end.span.end)
        } else if self.take(&TokenKind::LeftBrace).is_some() {
            let fields = self.parse_struct_fields()?;
            let end = self.expect_symbol(&TokenKind::RightBrace, "`}` after variant fields")?;
            (EnumVariantPayload::Struct(fields), end.span.end)
        } else {
            (EnumVariantPayload::Unit, name.span.end)
        };
        Some(EnumVariant {
            name,
            payload,
            span: Span::new(start, end),
        })
    }

    fn parse_type_alias(&mut self) -> Option<TypeAliasDeclaration> {
        let start = self.current().span.start;
        let is_public = self.take(&TokenKind::Pub).is_some();
        self.expect_symbol(&TokenKind::Type, "`type`")?;
        let name = self.expect_identifier("type alias name")?;
        self.expect_symbol(&TokenKind::Equal, "`=` after the type alias name")?;
        let target = self.parse_type_name()?;
        let end = self.expect_symbol(&TokenKind::Semicolon, "`;` after the type alias")?;
        Some(TypeAliasDeclaration {
            is_public,
            name,
            target,
            span: Span::new(start, end.span.end),
        })
    }

    fn parse_type_list(&mut self, end: &TokenKind) -> Option<Vec<TypeName>> {
        let mut types = Vec::new();
        if self.at(end) {
            return Some(types);
        }
        loop {
            types.push(self.parse_type_name()?);
            if self.take(&TokenKind::Comma).is_none() || self.at(end) {
                break;
            }
        }
        Some(types)
    }

    fn parse_function(&mut self, attributes: Vec<Attribute>) -> Option<Function> {
        self.parse_function_with_receiver(None, attributes)
    }

    fn parse_function_with_receiver(
        &mut self,
        receiver: Option<&TypeName>,
        attributes: Vec<Attribute>,
    ) -> Option<Function> {
        let start = attributes
            .first()
            .map_or(self.current().span.start, |attribute| attribute.span.start);
        let is_public = self.take(&TokenKind::Pub).is_some();
        let is_comptime = self.take(&TokenKind::Comptime).is_some();
        self.expect(
            &TokenKind::Fn,
            "E1001",
            "expected a function declaration",
            "insert `fn`",
        )?;
        let name = self.expect_callable_name("function name")?;
        let generic_parameters = self.parse_generic_parameters()?;
        self.expect_symbol(&TokenKind::LeftParen, "`(` after the function name")?;
        let parameters = self.parse_parameters(receiver)?;
        self.expect_symbol(&TokenKind::RightParen, "`)` after the parameters")?;
        let return_type = if self.take(&TokenKind::Arrow).is_some() {
            Some(self.parse_type_name()?)
        } else {
            None
        };
        let where_predicates = self.parse_where_clause()?;
        let body = self.parse_block()?;
        let span = Span::new(start, body.span.end);

        Some(Function {
            attributes,
            is_comptime,
            is_public,
            name,
            generic_parameters,
            parameters,
            return_type,
            where_predicates,
            body,
            span,
        })
    }

    fn parse_constant_declaration(&mut self) -> Option<ConstantDeclaration> {
        let start = self.current().span.start;
        let is_public = self.take(&TokenKind::Pub).is_some();
        self.expect_symbol(&TokenKind::Const, "`const`")?;
        let name = self.expect_identifier("constant name")?;
        self.expect_symbol(&TokenKind::Colon, "`:` after the constant name")?;
        let ty = self.parse_type_name()?;
        self.expect_symbol(&TokenKind::Equal, "`=` before the constant value")?;
        let value = self.parse_expression()?;
        let end = self.expect_symbol(&TokenKind::Semicolon, "`;` after the constant")?;
        Some(ConstantDeclaration {
            is_public,
            name,
            ty,
            value,
            span: Span::new(start, end.span.end),
        })
    }

    fn parse_static_declaration(&mut self) -> Option<StaticDeclaration> {
        let start = self.current().span.start;
        let is_public = self.take(&TokenKind::Pub).is_some();
        self.expect_symbol(&TokenKind::Static, "`static`")?;
        let mutable = self.take(&TokenKind::Mut).is_some();
        let name = self.expect_identifier("static name")?;
        self.expect_symbol(&TokenKind::Colon, "`:` after the static name")?;
        let ty = self.parse_type_name()?;
        self.expect_symbol(&TokenKind::Equal, "`=` before the static initializer")?;
        let value = self.parse_expression()?;
        let end = self.expect_symbol(&TokenKind::Semicolon, "`;` after the static")?;
        Some(StaticDeclaration {
            is_public,
            mutable,
            name,
            ty,
            value,
            span: Span::new(start, end.span.end),
        })
    }

    fn parse_comptime_block(&mut self) -> Option<ComptimeBlock> {
        let start = self.expect_symbol(&TokenKind::Comptime, "`comptime`")?;
        let body = self.parse_block()?;
        Some(ComptimeBlock {
            span: Span::new(start.span.start, body.span.end),
            body,
        })
    }

    fn parse_impl_declaration(&mut self) -> Option<ImplDeclaration> {
        let start = self.expect_symbol(&TokenKind::Impl, "`impl`")?.span.start;
        let generic_parameters = self.parse_generic_parameters()?;
        let first_type = self.parse_type_name()?;
        let (trait_type, target) = if self.take(&TokenKind::For).is_some() {
            (Some(first_type), self.parse_type_name()?)
        } else {
            (None, first_type)
        };
        let where_predicates = self.parse_where_clause()?;
        self.expect_symbol(&TokenKind::LeftBrace, "`{` before implementation methods")?;
        let mut methods = Vec::new();
        while !self.at(&TokenKind::RightBrace) && !self.at(&TokenKind::Eof) {
            let attributes = self.parse_attributes()?;
            if !(self.at(&TokenKind::Fn)
                || (self.at(&TokenKind::Pub) && self.at_offset(1, &TokenKind::Fn))
                || (self.at(&TokenKind::Comptime) && self.at_offset(1, &TokenKind::Fn))
                || (self.at(&TokenKind::Pub)
                    && self.at_offset(1, &TokenKind::Comptime)
                    && self.at_offset(2, &TokenKind::Fn)))
            {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E1006",
                        "inherent impl blocks contain methods only",
                        self.current().span,
                    )
                    .with_help("start the method with `fn` or `pub fn`"),
                );
                return None;
            }
            methods.push(self.parse_function_with_receiver(Some(&target), attributes)?);
        }
        let end = self.expect_symbol(&TokenKind::RightBrace, "`}` after implementation methods")?;
        Some(ImplDeclaration {
            generic_parameters,
            trait_type,
            target,
            where_predicates,
            methods,
            span: Span::new(start, end.span.end),
        })
    }

    fn parse_trait_declaration(&mut self) -> Option<TraitDeclaration> {
        let start = self.current().span.start;
        let is_public = self.take(&TokenKind::Pub).is_some();
        self.expect_symbol(&TokenKind::Trait, "`trait`")?;
        let name = self.expect_identifier("trait name")?;
        let generic_parameters = self.parse_generic_parameters()?;
        let supertraits = if self.take(&TokenKind::Colon).is_some() {
            self.parse_trait_bounds()?
        } else {
            Vec::new()
        };
        let where_predicates = self.parse_where_clause()?;
        self.expect_symbol(&TokenKind::LeftBrace, "`{` before trait methods")?;
        let receiver = TypeName {
            kind: TypeNameKind::Path(Path {
                segments: vec![Identifier {
                    name: "Self".to_owned(),
                    span: name.span,
                }],
                span: name.span,
            }),
            span: name.span,
        };
        let mut methods = Vec::new();
        while !self.at(&TokenKind::RightBrace) && !self.at(&TokenKind::Eof) {
            methods.push(self.parse_trait_method(&receiver)?);
        }
        let end = self.expect_symbol(&TokenKind::RightBrace, "`}` after trait methods")?;
        Some(TraitDeclaration {
            is_public,
            name,
            generic_parameters,
            supertraits,
            where_predicates,
            methods,
            span: Span::new(start, end.span.end),
        })
    }

    fn parse_trait_method(&mut self, receiver: &TypeName) -> Option<TraitMethod> {
        let start = self.expect_symbol(&TokenKind::Fn, "`fn` in trait declaration")?;
        let name = self.expect_identifier("trait method name")?;
        let generic_parameters = self.parse_generic_parameters()?;
        self.expect_symbol(&TokenKind::LeftParen, "`(` after the method name")?;
        let parameters = self.parse_parameters(Some(receiver))?;
        self.expect_symbol(&TokenKind::RightParen, "`)` after the parameters")?;
        let return_type = if self.take(&TokenKind::Arrow).is_some() {
            Some(self.parse_type_name()?)
        } else {
            None
        };
        let where_predicates = self.parse_where_clause()?;
        let end = self.expect_symbol(&TokenKind::Semicolon, "`;` after trait method signature")?;
        Some(TraitMethod {
            name,
            generic_parameters,
            parameters,
            return_type,
            where_predicates,
            span: Span::new(start.span.start, end.span.end),
        })
    }

    fn parse_extern_declarations(&mut self, attributes: &[Attribute]) -> Option<Vec<Item>> {
        let start = attributes
            .first()
            .map_or(self.current().span.start, |attribute| attribute.span.start);
        let link = self.parse_extern_link(attributes).ok()?;
        let is_public = self.take(&TokenKind::Pub).is_some();
        self.expect_symbol(&TokenKind::Extern, "`extern`")?;
        let abi_token = self.advance();
        let TokenKind::String(abi) = abi_token.kind else {
            self.diagnostics.push(
                Diagnostic::error(
                    "E1004",
                    "expected an ABI string after `extern`",
                    abi_token.span,
                )
                .with_help("write `extern \"C\" fn ...;`"),
            );
            return None;
        };
        if abi != "C" {
            self.diagnostics.push(
                Diagnostic::error(
                    "E1004",
                    format!("unsupported native ABI `{abi}`"),
                    abi_token.span,
                )
                .with_help("v0.1 supports only `extern \"C\"`"),
            );
        }
        if self.take(&TokenKind::LeftBrace).is_some() {
            if is_public {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E1005",
                        "visibility belongs on declarations inside an extern block",
                        Span::new(start, abi_token.span.end),
                    )
                    .with_help("move `pub` before the functions that should be re-exported"),
                );
            }
            let mut declarations = Vec::new();
            while !self.at(&TokenKind::RightBrace) && !self.at(&TokenKind::Eof) {
                let function_start = self.current().span.start;
                let function_public = self.take(&TokenKind::Pub).is_some();
                let function = self.parse_extern_function_signature(
                    function_start,
                    function_public,
                    &abi,
                    link.as_deref(),
                )?;
                declarations.push(Item::ExternFunction(function));
            }
            self.expect_symbol(&TokenKind::RightBrace, "`}` after the extern block")?;
            return Some(declarations);
        }
        let function =
            self.parse_extern_function_signature(start, is_public, &abi, link.as_deref())?;
        Some(vec![Item::ExternFunction(function)])
    }

    fn parse_extern_link(&mut self, attributes: &[Attribute]) -> Result<Option<String>, ()> {
        if attributes.is_empty() {
            return Ok(None);
        }
        if attributes.len() != 1 || attributes[0].name.name != "link" {
            self.diagnostics.push(
                Diagnostic::error(
                    "E1020",
                    "native declarations accept only `@link(\"library\")`",
                    attributes[0].span,
                )
                .with_help("remove other attributes from the native declaration"),
            );
            return Err(());
        }
        let [AttributeArgument::String(library)] = attributes[0].arguments.as_slice() else {
            self.diagnostics.push(
                Diagnostic::error(
                    "E1020",
                    "`@link` expects exactly one string literal",
                    attributes[0].span,
                )
                .with_help("write `@link(\"library\")`"),
            );
            return Err(());
        };
        Ok(Some(library.value.clone()))
    }

    fn parse_extern_function_signature(
        &mut self,
        start: usize,
        is_public: bool,
        abi: &str,
        link: Option<&str>,
    ) -> Option<ExternFunction> {
        self.expect_symbol(&TokenKind::Fn, "`fn` in the extern declaration")?;
        let name = self.expect_identifier("external function name")?;
        self.expect_symbol(&TokenKind::LeftParen, "`(` after the function name")?;
        let parameters = self.parse_parameters(None)?;
        self.expect_symbol(&TokenKind::RightParen, "`)` after the parameters")?;
        let return_type = if self.take(&TokenKind::Arrow).is_some() {
            Some(self.parse_type_name()?)
        } else {
            None
        };
        let end = self.expect_symbol(
            &TokenKind::Semicolon,
            "`;` after the external function declaration",
        )?;
        let symbol = name.name.clone();
        Some(ExternFunction {
            is_public,
            abi: abi.to_owned(),
            name,
            symbol,
            link: link.map(str::to_owned),
            parameters,
            return_type,
            span: Span::new(start, end.span.end),
        })
    }

    fn parse_parameters(&mut self, receiver: Option<&TypeName>) -> Option<Vec<Parameter>> {
        let mut parameters = Vec::new();
        if self.at(&TokenKind::RightParen) {
            return Some(parameters);
        }

        loop {
            match self.parse_self_parameter(receiver) {
                Ok(Some(parameter)) => {
                    parameters.push(parameter);
                    if self.take(&TokenKind::Comma).is_none() {
                        break;
                    }
                    if self.at(&TokenKind::RightParen) {
                        break;
                    }
                    continue;
                }
                Ok(None) => {}
                Err(()) => return None,
            }
            let name = self.expect_identifier("parameter name")?;
            let start = name.span.start;
            self.expect_symbol(&TokenKind::Colon, "`:` after the parameter name")?;
            let ty = self.parse_type_name()?;
            let end = ty.span.end;
            parameters.push(Parameter {
                name,
                ty,
                span: Span::new(start, end),
            });

            if self.take(&TokenKind::Comma).is_none() {
                break;
            }
            if self.at(&TokenKind::RightParen) {
                break;
            }
        }

        Some(parameters)
    }

    fn parse_self_parameter(
        &mut self,
        receiver: Option<&TypeName>,
    ) -> Result<Option<Parameter>, ()> {
        let Some(receiver) = receiver else {
            return Ok(None);
        };
        let start = self.current().span.start;
        let (mutable, borrowed) = if self.take(&TokenKind::Ampersand).is_some() {
            (self.take(&TokenKind::Mut).is_some(), true)
        } else {
            (false, false)
        };
        let TokenKind::Identifier(name) = &self.current().kind else {
            if borrowed {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E1006",
                        "expected `self` after receiver borrow",
                        self.current().span,
                    )
                    .with_help("write `&self` or `&mut self`"),
                );
                return Err(());
            }
            return Ok(None);
        };
        if name != "self" {
            if borrowed {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E1006",
                        "only `self` may omit its type in a method",
                        self.current().span,
                    )
                    .with_help("name and type ordinary parameters as `name: Type`"),
                );
                return Err(());
            }
            return Ok(None);
        }
        let self_token = self.advance();
        let ty = if borrowed {
            TypeName {
                kind: TypeNameKind::Reference {
                    mutable,
                    target: Box::new(receiver.clone()),
                },
                span: Span::new(start, self_token.span.end),
            }
        } else {
            receiver.clone()
        };
        Ok(Some(Parameter {
            name: Identifier {
                name: "self".to_owned(),
                span: self_token.span,
            },
            ty,
            span: Span::new(start, self_token.span.end),
        }))
    }

    fn parse_import(&mut self) -> Option<ImportDeclaration> {
        let start = self.current().span.start;
        let is_public = self.take(&TokenKind::Pub).is_some();

        let kind = if self.take(&TokenKind::From).is_some() {
            let module = self.parse_path()?;
            self.expect_symbol(&TokenKind::Import, "`import` after the source module path")?;
            let names = self.parse_import_names()?;
            ImportKind::Symbols { module, names }
        } else if self.take(&TokenKind::Import).is_some() {
            let path = self.parse_path()?;
            let alias = self.parse_optional_alias();
            ImportKind::Module { path, alias }
        } else {
            let token = self.current();
            self.diagnostics.push(
                Diagnostic::error(
                    "E1007",
                    "expected `from` or `import` after `pub`",
                    token.span,
                )
                .with_help("use `pub from x::y import z;` or `pub import x::y;`"),
            );
            return None;
        };

        let end = self.expect_symbol(&TokenKind::Semicolon, "`;` after the import")?;
        Some(ImportDeclaration {
            is_public,
            kind,
            span: Span::new(start, end.span.end),
        })
    }

    fn parse_path(&mut self) -> Option<Path> {
        let first = self.expect_identifier("a path segment")?;
        let start = first.span.start;
        let mut end = first.span.end;
        let mut segments = vec![first];

        while self.at(&TokenKind::ColonColon) && !self.at_offset(1, &TokenKind::Less) {
            self.advance();
            let segment = self.expect_associated_path_segment()?;
            end = segment.span.end;
            segments.push(segment);
        }

        Some(Path {
            segments,
            span: Span::new(start, end),
        })
    }

    fn expect_associated_path_segment(&mut self) -> Option<Identifier> {
        if self.at(&TokenKind::From) {
            let token = self.advance();
            return Some(Identifier {
                name: "from".to_owned(),
                span: token.span,
            });
        }
        self.expect_identifier("a path segment after `::`")
    }

    fn parse_import_names(&mut self) -> Option<Vec<ImportedName>> {
        let mut names = Vec::new();

        loop {
            let name = self.expect_identifier("an imported name")?;
            let alias = self.parse_optional_alias();
            names.push(ImportedName { name, alias });

            if self.take(&TokenKind::Comma).is_none() {
                break;
            }
            if self.at(&TokenKind::Semicolon) {
                break;
            }
        }

        Some(names)
    }

    fn parse_optional_alias(&mut self) -> Option<Identifier> {
        self.take(&TokenKind::As)?;
        self.expect_identifier("an alias after `as`")
    }

    fn parse_generic_parameters(&mut self) -> Option<Vec<GenericParameter>> {
        if self.take(&TokenKind::Less).is_none() {
            return Some(Vec::new());
        }
        let mut parameters = Vec::new();
        loop {
            let start = self.current().span.start;
            let parameter = if self.take(&TokenKind::Ellipsis).is_some() {
                let name = self.expect_identifier("type pack parameter name")?;
                let bounds = if self.take(&TokenKind::Colon).is_some() {
                    self.parse_trait_bounds()?
                } else {
                    Vec::new()
                };
                let end = bounds.last().map_or(name.span.end, |bound| bound.span.end);
                GenericParameter::TypePack {
                    name,
                    bounds,
                    span: Span::new(start, end),
                }
            } else if self.take(&TokenKind::Const).is_some() {
                let name = self.expect_identifier("const generic parameter name")?;
                self.expect_symbol(&TokenKind::Colon, "`:` after const parameter name")?;
                let ty = self.parse_type_name()?;
                let default = if self.take(&TokenKind::Equal).is_some() {
                    Some(self.parse_const_argument_expression()?)
                } else {
                    None
                };
                let end = default
                    .as_ref()
                    .map_or(ty.span.end, |value| value.span().end);
                GenericParameter::Const {
                    name,
                    ty,
                    default,
                    span: Span::new(start, end),
                }
            } else {
                let name = self.expect_identifier("type parameter name")?;
                let bounds = if self.take(&TokenKind::Colon).is_some() {
                    self.parse_trait_bounds()?
                } else {
                    Vec::new()
                };
                let default = if self.take(&TokenKind::Equal).is_some() {
                    Some(self.parse_type_name()?)
                } else {
                    None
                };
                let end = default.as_ref().map_or_else(
                    || bounds.last().map_or(name.span.end, |bound| bound.span.end),
                    |ty| ty.span.end,
                );
                GenericParameter::Type {
                    name,
                    bounds,
                    default,
                    span: Span::new(start, end),
                }
            };
            parameters.push(parameter);
            if self.take(&TokenKind::Comma).is_none() {
                break;
            }
            if self.at_type_greater() {
                break;
            }
        }
        self.expect_type_greater()?;
        Some(parameters)
    }

    fn parse_where_clause(&mut self) -> Option<Vec<WherePredicate>> {
        if self.take(&TokenKind::Where).is_none() {
            return Some(Vec::new());
        }
        let mut predicates = Vec::new();
        loop {
            let ty = self.parse_type_name()?;
            let start = ty.span.start;
            self.expect_symbol(&TokenKind::Colon, "`:` in where predicate")?;
            let bounds = self.parse_trait_bounds()?;
            let end = bounds.last().map_or(ty.span.end, |bound| bound.span.end);
            predicates.push(WherePredicate {
                ty,
                bounds,
                span: Span::new(start, end),
            });
            if self.take(&TokenKind::Comma).is_none()
                || matches!(
                    self.current().kind,
                    TokenKind::LeftBrace | TokenKind::Semicolon
                )
            {
                break;
            }
        }
        Some(predicates)
    }

    fn parse_trait_bounds(&mut self) -> Option<Vec<Path>> {
        let mut bounds = vec![self.parse_path()?];
        while self.take(&TokenKind::Plus).is_some() {
            bounds.push(self.parse_path()?);
        }
        Some(bounds)
    }

    fn parse_const_argument_expression(&mut self) -> Option<Expression> {
        if self.take(&TokenKind::LeftBrace).is_some() {
            let value = self.parse_expression()?;
            self.expect_symbol(&TokenKind::RightBrace, "`}` after const argument")?;
            return Some(value);
        }
        self.parse_unary()
    }

    fn at_type_greater(&self) -> bool {
        self.pending_type_greater > 0
            || matches!(
                self.current().kind,
                TokenKind::Greater | TokenKind::RightShift
            )
    }

    fn parse_type_name(&mut self) -> Option<TypeName> {
        self.with_nesting(Self::parse_type_name_inner)
    }

    fn parse_type_name_inner(&mut self) -> Option<TypeName> {
        let type_start = self.current().span.start;
        if self.take(&TokenKind::Ellipsis).is_some() {
            let pack = self.expect_identifier("type pack name after `...`")?;
            let template = if self.take(&TokenKind::FatArrow).is_some() {
                Some(Box::new(self.parse_type_name()?))
            } else {
                None
            };
            let end = template.as_ref().map_or(pack.span.end, |ty| ty.span.end);
            return Some(TypeName {
                kind: TypeNameKind::PackExpansion { pack, template },
                span: Span::new(type_start, end),
            });
        }
        if self.at(&TokenKind::Fn) {
            return self.parse_function_type();
        }
        if self.take(&TokenKind::Ampersand).is_some() {
            let mutable = self.take(&TokenKind::Mut).is_some();
            let target = Box::new(self.parse_type_name()?);
            return Some(TypeName {
                span: Span::new(type_start, target.span.end),
                kind: TypeNameKind::Reference { mutable, target },
            });
        }
        if self.take(&TokenKind::Star).is_some() {
            let mutable = if self.take(&TokenKind::Mut).is_some() {
                true
            } else {
                self.expect_symbol(&TokenKind::Const, "`const` or `mut` after `*`")?;
                false
            };
            let target = Box::new(self.parse_type_name()?);
            return Some(TypeName {
                span: Span::new(type_start, target.span.end),
                kind: TypeNameKind::RawPointer { mutable, target },
            });
        }
        if let Some(start) = self.take(&TokenKind::LeftParen) {
            if let Some(end) = self.take(&TokenKind::RightParen) {
                return Some(TypeName {
                    kind: TypeNameKind::Unit,
                    span: Span::new(start.span.start, end.span.end),
                });
            }
            let elements = self.parse_type_list(&TokenKind::RightParen)?;
            let end = self.expect_symbol(&TokenKind::RightParen, "`)` in the tuple type")?;
            return Some(TypeName {
                kind: TypeNameKind::Tuple(elements),
                span: Span::new(start.span.start, end.span.end),
            });
        }
        if let Some(start) = self.take(&TokenKind::LeftBracket) {
            let element = Box::new(self.parse_type_name()?);
            let kind = if self.take(&TokenKind::Semicolon).is_some() {
                let length = Box::new(self.parse_expression()?);
                TypeNameKind::Array { element, length }
            } else {
                TypeNameKind::Slice(element)
            };
            let end = self.expect_symbol(&TokenKind::RightBracket, "`]` after the array type")?;
            return Some(TypeName {
                kind,
                span: Span::new(start.span.start, end.span.end),
            });
        }
        let path = self.parse_path()?;
        if self.take(&TokenKind::Less).is_some() {
            let arguments = self.parse_generic_argument_list()?;
            self.expect_type_greater()?;
            let end = self.previous_type_end(path.span.end);
            return Some(TypeName {
                kind: TypeNameKind::Generic { path, arguments },
                span: Span::new(type_start, end),
            });
        }
        Some(TypeName {
            span: path.span,
            kind: TypeNameKind::Path(path),
        })
    }

    fn parse_function_type(&mut self) -> Option<TypeName> {
        let start = self.expect_symbol(&TokenKind::Fn, "`fn` in a function type")?;
        self.expect_symbol(&TokenKind::LeftParen, "`(` after `fn` in a function type")?;
        let parameters = if self.at(&TokenKind::RightParen) {
            Vec::new()
        } else {
            self.parse_type_list(&TokenKind::RightParen)?
        };
        self.expect_symbol(&TokenKind::RightParen, "`)` after function type parameters")?;
        self.expect_symbol(&TokenKind::Arrow, "`->` in a function type")?;
        let return_type = Box::new(self.parse_type_name()?);
        Some(TypeName {
            span: Span::new(start.span.start, return_type.span.end),
            kind: TypeNameKind::Function {
                parameters,
                return_type,
            },
        })
    }

    fn parse_block(&mut self) -> Option<Block> {
        let start = self.expect_symbol(&TokenKind::LeftBrace, "`{` to start the block")?;
        let mut statements = Vec::new();
        let mut tail = None;

        while !self.at(&TokenKind::RightBrace) && !self.at(&TokenKind::Eof) {
            let statement = if self.at(&TokenKind::Let) {
                self.parse_let_statement().map(Statement::Let)
            } else if self.at(&TokenKind::Defer) {
                self.parse_defer_statement().map(Statement::Defer)
            } else if self.at(&TokenKind::Return) {
                self.parse_return_statement().map(Statement::Return)
            } else if self.at(&TokenKind::While) {
                self.parse_while_statement().map(Statement::While)
            } else if self.at(&TokenKind::For) {
                self.parse_for_statement().map(Statement::For)
            } else if self.at(&TokenKind::Break) {
                self.parse_loop_control_statement(true)
            } else if self.at(&TokenKind::Continue) {
                self.parse_loop_control_statement(false)
            } else {
                let expression = self.parse_expression()?;
                if let Some(end) = self.take(&TokenKind::Semicolon) {
                    let span = Span::new(expression.span().start, end.span.end);
                    Some(Statement::Expression(ExpressionStatement {
                        expression,
                        span,
                    }))
                } else if self.at(&TokenKind::RightBrace) {
                    tail = Some(Box::new(expression));
                    break;
                } else if matches!(
                    &expression,
                    Expression::If(_)
                        | Expression::Match(_)
                        | Expression::Loop(_)
                        | Expression::Unsafe(_)
                        | Expression::Block(_)
                ) {
                    let span = expression.span();
                    Some(Statement::Expression(ExpressionStatement {
                        expression,
                        span,
                    }))
                } else {
                    self.diagnostics.push(
                        Diagnostic::error(
                            "E1011",
                            "expected `;` after expression or `}` after block value",
                            self.current().span,
                        )
                        .with_help("terminate the statement with `;`"),
                    );
                    None
                }
            };

            match statement {
                Some(statement) => statements.push(statement),
                None => self.synchronize_statement(),
            }
        }

        let end = self.expect_symbol(&TokenKind::RightBrace, "`}` to close the block")?;
        Some(Block {
            statements,
            tail,
            span: Span::new(start.span.start, end.span.end),
        })
    }

    fn parse_let_statement(&mut self) -> Option<LetStatement> {
        let start = self.advance();
        let mutable = self.take(&TokenKind::Mut).is_some();
        let name = self.expect_identifier("binding name")?;
        let ty = if self.take(&TokenKind::Colon).is_some() {
            Some(self.parse_type_name()?)
        } else {
            None
        };
        self.expect_symbol(&TokenKind::Equal, "`=` before the initializer")?;
        let initializer = self.parse_expression()?;
        let end = self.expect_symbol(&TokenKind::Semicolon, "`;` after the binding")?;
        Some(LetStatement {
            mutable,
            name,
            ty,
            initializer,
            span: Span::new(start.span.start, end.span.end),
        })
    }

    fn parse_defer_statement(&mut self) -> Option<DeferStatement> {
        let start = self.advance();
        if self.at(&TokenKind::LeftBrace) {
            let block = self.parse_block()?;
            let span = Span::new(start.span.start, block.span.end);
            return Some(DeferStatement {
                action: Expression::Block(Box::new(block)),
                span,
            });
        }
        let action = self.parse_expression()?;
        let end = self.expect_symbol(&TokenKind::Semicolon, "`;` after `defer`")?;
        Some(DeferStatement {
            action,
            span: Span::new(start.span.start, end.span.end),
        })
    }

    fn parse_return_statement(&mut self) -> Option<ReturnStatement> {
        let start = self.advance();
        let value = if self.at(&TokenKind::Semicolon) {
            None
        } else {
            Some(self.parse_expression()?)
        };
        let end = self.expect_symbol(&TokenKind::Semicolon, "`;` after `return`")?;
        Some(ReturnStatement {
            value,
            span: Span::new(start.span.start, end.span.end),
        })
    }

    fn parse_while_statement(&mut self) -> Option<WhileStatement> {
        let start = self.advance();
        let condition = self.parse_condition_expression()?;
        let body = self.parse_block()?;
        let span = Span::new(start.span.start, body.span.end);
        Some(WhileStatement {
            condition,
            body,
            span,
        })
    }

    fn parse_for_statement(&mut self) -> Option<ForStatement> {
        let start = self.advance();
        let pattern = self.parse_pattern()?;
        self.expect_symbol(&TokenKind::In, "`in` after the loop pattern")?;
        let iterable = self.parse_condition_expression()?;
        let body = self.parse_block()?;
        let span = Span::new(start.span.start, body.span.end);
        Some(ForStatement {
            pattern,
            iterable,
            body,
            span,
        })
    }

    fn parse_loop_control_statement(&mut self, is_break: bool) -> Option<Statement> {
        let start = self.advance();
        let value = if is_break && !self.at(&TokenKind::Semicolon) {
            Some(self.parse_expression()?)
        } else {
            None
        };
        let end = self.expect_symbol(
            &TokenKind::Semicolon,
            if is_break {
                "`;` after `break`"
            } else {
                "`;` after `continue`"
            },
        )?;
        let span = Span::new(start.span.start, end.span.end);
        Some(if is_break {
            Statement::Break(BreakStatement { value, span })
        } else {
            Statement::Continue(span)
        })
    }

    fn parse_expression(&mut self) -> Option<Expression> {
        self.with_nesting(Self::parse_assignment)
    }

    fn parse_condition_expression(&mut self) -> Option<Expression> {
        let previous = self.allow_struct_expression;
        self.allow_struct_expression = false;
        let expression = self.parse_expression();
        self.allow_struct_expression = previous;
        expression
    }

    fn parse_assignment(&mut self) -> Option<Expression> {
        let target = self.parse_logic_or()?;
        let operator = if self.take(&TokenKind::Equal).is_some() {
            Some(AssignmentOperator::Assign)
        } else if self.take(&TokenKind::PlusEqual).is_some() {
            Some(AssignmentOperator::Add)
        } else if self.take(&TokenKind::MinusEqual).is_some() {
            Some(AssignmentOperator::Subtract)
        } else if self.take(&TokenKind::StarEqual).is_some() {
            Some(AssignmentOperator::Multiply)
        } else if self.take(&TokenKind::SlashEqual).is_some() {
            Some(AssignmentOperator::Divide)
        } else if self.take(&TokenKind::PercentEqual).is_some() {
            Some(AssignmentOperator::Remainder)
        } else if self.take(&TokenKind::AmpersandEqual).is_some() {
            Some(AssignmentOperator::BitAnd)
        } else if self.take(&TokenKind::CaretEqual).is_some() {
            Some(AssignmentOperator::BitXor)
        } else if self.take(&TokenKind::PipeEqual).is_some() {
            Some(AssignmentOperator::BitOr)
        } else if self.take(&TokenKind::LeftShiftEqual).is_some() {
            Some(AssignmentOperator::ShiftLeft)
        } else if self.take(&TokenKind::RightShiftEqual).is_some() {
            Some(AssignmentOperator::ShiftRight)
        } else {
            None
        };

        let Some(operator) = operator else {
            return Some(target);
        };
        let value = self.parse_expression()?;
        let span = Span::new(target.span().start, value.span().end);
        Some(Expression::Assignment(Box::new(AssignmentExpression {
            target,
            operator,
            value,
            span,
        })))
    }

    fn parse_logic_or(&mut self) -> Option<Expression> {
        let mut expression = self.parse_logic_and()?;
        while self.take(&TokenKind::PipePipe).is_some() {
            let right = self.parse_logic_and()?;
            expression = binary_expression(BinaryOperator::Or, expression, right);
        }
        Some(expression)
    }

    fn parse_logic_and(&mut self) -> Option<Expression> {
        let mut expression = self.parse_equality()?;
        while self.take(&TokenKind::AmpAmp).is_some() {
            let right = self.parse_equality()?;
            expression = binary_expression(BinaryOperator::And, expression, right);
        }
        Some(expression)
    }

    fn parse_equality(&mut self) -> Option<Expression> {
        let expression = self.parse_comparison()?;
        let operator = if self.take(&TokenKind::EqualEqual).is_some() {
            Some(BinaryOperator::Equal)
        } else if self.take(&TokenKind::BangEqual).is_some() {
            Some(BinaryOperator::NotEqual)
        } else {
            None
        };

        if let Some(operator) = operator {
            let right = self.parse_comparison()?;
            Some(binary_expression(operator, expression, right))
        } else {
            Some(expression)
        }
    }

    fn parse_comparison(&mut self) -> Option<Expression> {
        let expression = self.parse_bit_or()?;
        let operator = if self.take(&TokenKind::Less).is_some() {
            Some(BinaryOperator::Less)
        } else if self.take(&TokenKind::LessEqual).is_some() {
            Some(BinaryOperator::LessEqual)
        } else if self.take(&TokenKind::Greater).is_some() {
            Some(BinaryOperator::Greater)
        } else if self.take(&TokenKind::GreaterEqual).is_some() {
            Some(BinaryOperator::GreaterEqual)
        } else {
            None
        };

        if let Some(operator) = operator {
            let right = self.parse_bit_or()?;
            Some(binary_expression(operator, expression, right))
        } else {
            Some(expression)
        }
    }

    fn parse_bit_or(&mut self) -> Option<Expression> {
        let mut expression = self.parse_bit_xor()?;
        while self.take(&TokenKind::Pipe).is_some() {
            let right = self.parse_bit_xor()?;
            expression = binary_expression(BinaryOperator::BitOr, expression, right);
        }
        Some(expression)
    }

    fn parse_bit_xor(&mut self) -> Option<Expression> {
        let mut expression = self.parse_bit_and()?;
        while self.take(&TokenKind::Caret).is_some() {
            let right = self.parse_bit_and()?;
            expression = binary_expression(BinaryOperator::BitXor, expression, right);
        }
        Some(expression)
    }

    fn parse_bit_and(&mut self) -> Option<Expression> {
        let mut expression = self.parse_shift()?;
        while self.take(&TokenKind::Ampersand).is_some() {
            let right = self.parse_shift()?;
            expression = binary_expression(BinaryOperator::BitAnd, expression, right);
        }
        Some(expression)
    }

    fn parse_shift(&mut self) -> Option<Expression> {
        let mut expression = self.parse_additive()?;
        loop {
            let operator = if self.take(&TokenKind::LeftShift).is_some() {
                Some(BinaryOperator::ShiftLeft)
            } else if self.take(&TokenKind::RightShift).is_some() {
                Some(BinaryOperator::ShiftRight)
            } else {
                None
            };
            let Some(operator) = operator else {
                break;
            };
            let right = self.parse_additive()?;
            expression = binary_expression(operator, expression, right);
        }
        Some(expression)
    }

    fn parse_additive(&mut self) -> Option<Expression> {
        let mut expression = self.parse_multiplicative()?;
        loop {
            let operator = if self.take(&TokenKind::Plus).is_some() {
                Some(BinaryOperator::Add)
            } else if self.take(&TokenKind::Minus).is_some() {
                Some(BinaryOperator::Subtract)
            } else {
                None
            };
            let Some(operator) = operator else {
                break;
            };
            let right = self.parse_multiplicative()?;
            expression = binary_expression(operator, expression, right);
        }
        Some(expression)
    }

    fn parse_multiplicative(&mut self) -> Option<Expression> {
        let mut expression = self.parse_cast()?;
        loop {
            let operator = if self.take(&TokenKind::Star).is_some() {
                Some(BinaryOperator::Multiply)
            } else if self.take(&TokenKind::Slash).is_some() {
                Some(BinaryOperator::Divide)
            } else if self.take(&TokenKind::Percent).is_some() {
                Some(BinaryOperator::Remainder)
            } else {
                None
            };
            let Some(operator) = operator else {
                break;
            };
            let right = self.parse_cast()?;
            expression = binary_expression(operator, expression, right);
        }
        Some(expression)
    }

    fn parse_cast(&mut self) -> Option<Expression> {
        let mut expression = self.parse_unary()?;
        while self.take(&TokenKind::As).is_some() {
            let target = self.parse_type_name()?;
            let span = Span::new(expression.span().start, target.span.end);
            expression = Expression::Cast(Box::new(CastExpression {
                value: expression,
                target,
                span,
            }));
        }
        Some(expression)
    }

    fn parse_unary(&mut self) -> Option<Expression> {
        self.with_nesting(Self::parse_unary_inner)
    }

    fn parse_unary_inner(&mut self) -> Option<Expression> {
        let start = self.current().span.start;
        let operator = if self.take(&TokenKind::Minus).is_some() {
            Some(UnaryOperator::Negate)
        } else if self.take(&TokenKind::Bang).is_some() {
            Some(UnaryOperator::Not)
        } else if self.take(&TokenKind::Ampersand).is_some() {
            if self.take(&TokenKind::Mut).is_some() {
                Some(UnaryOperator::BorrowMut)
            } else {
                Some(UnaryOperator::Borrow)
            }
        } else if self.take(&TokenKind::Star).is_some() {
            Some(UnaryOperator::Dereference)
        } else {
            None
        };

        let Some(operator) = operator else {
            return self.parse_postfix();
        };
        let operand = self.parse_unary()?;
        let span = Span::new(start, operand.span().end);
        Some(Expression::Unary(Box::new(UnaryExpression {
            operator,
            operand,
            span,
        })))
    }

    fn parse_postfix(&mut self) -> Option<Expression> {
        let mut expression = self.parse_primary()?;

        loop {
            if let Expression::Path(path) = &expression
                && self.at(&TokenKind::ColonColon)
                && self.at_offset(1, &TokenKind::Less)
            {
                let path = path.clone();
                expression = self.parse_explicit_generic_function(path)?;
                continue;
            }
            let has_explicit_generic_arguments =
                matches!(&expression, Expression::Path(_) | Expression::Field(_))
                    && self.generic_call_arguments_follow();
            let generic_arguments = if has_explicit_generic_arguments {
                self.expect_symbol(&TokenKind::Less, "`<` before generic call arguments")?;
                let arguments = self.parse_generic_argument_list()?;
                self.expect_type_greater()?;
                arguments
            } else {
                Vec::new()
            };
            if self.at(&TokenKind::LeftParen) {
                expression = self.parse_call_postfix(
                    expression,
                    generic_arguments,
                    has_explicit_generic_arguments,
                )?;
            } else if self.take(&TokenKind::LeftBracket).is_some() {
                let start = expression.span().start;
                let indices = self.parse_expression_list(&TokenKind::RightBracket)?;
                if indices.is_empty() {
                    self.diagnostics.push(Diagnostic::error(
                        "E1013",
                        "index expression requires at least one index",
                        self.current().span,
                    ));
                }
                let end = self.expect_symbol(&TokenKind::RightBracket, "`]` after indices")?;
                expression = Expression::Index(Box::new(IndexExpression {
                    base: expression,
                    indices,
                    span: Span::new(start, end.span.end),
                }));
            } else if self.take(&TokenKind::Dot).is_some() {
                let start = expression.span().start;
                let end = self.current().span.end;
                let field = self.parse_field_name()?;
                expression = Expression::Field(Box::new(FieldExpression {
                    base: expression,
                    field,
                    span: Span::new(start, end),
                }));
            } else if let Some(question) = self.take(&TokenKind::Question) {
                let start = expression.span().start;
                expression = Expression::Try {
                    value: Box::new(expression),
                    span: Span::new(start, question.span.end),
                };
            } else {
                break;
            }
        }

        Some(expression)
    }

    fn parse_explicit_generic_function(&mut self, path: Path) -> Option<Expression> {
        self.expect_symbol(
            &TokenKind::ColonColon,
            "`::` before explicit generic arguments",
        )?;
        self.expect_symbol(&TokenKind::Less, "`<` before generic arguments")?;
        let generic_arguments = self.parse_generic_argument_list()?;
        self.expect_type_greater()?;
        let end = self.previous_type_end(path.span.end);

        if self.at(&TokenKind::LeftParen) {
            return self.parse_call_postfix(Expression::Path(path), generic_arguments, true);
        }

        let start = path.span.start;
        Some(Expression::GenericFunction(Box::new(
            GenericFunctionExpression {
                path,
                generic_arguments,
                span: Span::new(start, end),
            },
        )))
    }

    fn parse_call_postfix(
        &mut self,
        callee: Expression,
        generic_arguments: Vec<GenericArgument>,
        has_explicit_generic_arguments: bool,
    ) -> Option<Expression> {
        let start = callee.span().start;
        self.expect_symbol(&TokenKind::LeftParen, "`(` before call arguments")?;
        let arguments = self.parse_arguments()?;
        let end = self.expect_symbol(&TokenKind::RightParen, "`)` after call arguments")?;
        Some(Expression::Call(Box::new(CallExpression {
            callee,
            generic_arguments,
            has_explicit_generic_arguments,
            arguments,
            span: Span::new(start, end.span.end),
        })))
    }

    fn parse_field_name(&mut self) -> Option<FieldName> {
        let token = self.current().clone();
        match token.kind {
            TokenKind::Identifier(name) => {
                self.advance();
                Some(FieldName::Named(Identifier {
                    name,
                    span: token.span,
                }))
            }
            TokenKind::Integer(spelling) => {
                self.advance();
                let normalized = spelling.replace('_', "");
                let Ok(index) = normalized.parse::<u32>() else {
                    self.diagnostics.push(Diagnostic::error(
                        "E1004",
                        "tuple field index is too large",
                        token.span,
                    ));
                    return None;
                };
                Some(FieldName::TupleIndex {
                    index,
                    span: token.span,
                })
            }
            _ => {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E1005",
                        "expected a field name or tuple index after `.`",
                        token.span,
                    )
                    .with_help("use `.field` or `.0`"),
                );
                None
            }
        }
    }

    fn parse_generic_argument_list(&mut self) -> Option<Vec<GenericArgument>> {
        let mut arguments = Vec::new();
        if self.at_type_greater() {
            return Some(arguments);
        }
        loop {
            let argument = if let Some(start) = self.take(&TokenKind::Ellipsis) {
                let pack = self.expect_identifier("type pack name after `...`")?;
                let template = if self.take(&TokenKind::FatArrow).is_some() {
                    Some(self.parse_type_name()?)
                } else {
                    None
                };
                let end = template.as_ref().map_or(pack.span.end, |ty| ty.span.end);
                GenericArgument::Pack {
                    pack,
                    template,
                    span: Span::new(start.span.start, end),
                }
            } else if matches!(
                self.current().kind,
                TokenKind::Integer(_)
                    | TokenKind::True
                    | TokenKind::False
                    | TokenKind::Character(_)
                    | TokenKind::Minus
                    | TokenKind::Bang
                    | TokenKind::LeftBrace
            ) {
                GenericArgument::Const(self.parse_const_argument_expression()?)
            } else {
                GenericArgument::Type(self.parse_type_name()?)
            };
            arguments.push(argument);
            if self.at_type_greater() {
                break;
            }
            if self.take(&TokenKind::Comma).is_none() || self.at_type_greater() {
                break;
            }
        }
        Some(arguments)
    }

    fn generic_call_arguments_follow(&self) -> bool {
        if !self.at(&TokenKind::Less) {
            return false;
        }
        let mut depth = 0_i32;
        let mut offset = 0;
        loop {
            match self.token_at(offset).kind {
                TokenKind::Less => depth += 1,
                TokenKind::Greater => depth -= 1,
                TokenKind::RightShift => depth -= 2,
                TokenKind::Eof | TokenKind::Semicolon | TokenKind::LeftBrace => return false,
                _ => {}
            }
            if depth <= 0 {
                return offset > 0 && self.at_offset(offset + 1, &TokenKind::LeftParen);
            }
            offset += 1;
        }
    }

    fn parse_arguments(&mut self) -> Option<Vec<Expression>> {
        self.parse_expression_list(&TokenKind::RightParen)
    }

    fn parse_expression_list(&mut self, end: &TokenKind) -> Option<Vec<Expression>> {
        let mut arguments = Vec::new();
        if self.at(end) {
            return Some(arguments);
        }

        loop {
            arguments.push(self.parse_expression()?);
            if self.take(&TokenKind::Comma).is_none() {
                break;
            }
            if self.at(end) {
                break;
            }
        }
        Some(arguments)
    }

    fn parse_primary(&mut self) -> Option<Expression> {
        let token = self.current().clone();
        match token.kind {
            TokenKind::Integer(spelling) => {
                self.advance();
                self.parse_integer_literal(&spelling, token.span)
            }
            TokenKind::Float(spelling) => {
                self.advance();
                self.parse_float_literal(&spelling, token.span)
            }
            TokenKind::Character(value) => {
                self.advance();
                Some(Expression::Character(CharacterLiteral {
                    value,
                    span: token.span,
                }))
            }
            TokenKind::String(value) => {
                self.advance();
                Some(Expression::String(StringLiteral {
                    value,
                    span: token.span,
                }))
            }
            TokenKind::FormattedString(fragments) => {
                self.advance();
                self.parse_formatted_string(fragments, token.span)
            }
            TokenKind::CString(value) => {
                self.advance();
                Some(Expression::CString(StringLiteral {
                    value,
                    span: token.span,
                }))
            }
            TokenKind::True | TokenKind::False => {
                self.advance();
                Some(Expression::Boolean(BooleanLiteral {
                    value: matches!(token.kind, TokenKind::True),
                    span: token.span,
                }))
            }
            TokenKind::Identifier(_) => {
                let path = self.parse_path()?;
                if self.allow_struct_expression && self.at(&TokenKind::LeftBrace) {
                    self.parse_struct_expression(path)
                } else {
                    Some(Expression::Path(path))
                }
            }
            TokenKind::Ellipsis => self.parse_pack_expansion_expression(),
            TokenKind::If => self.parse_if_expression(),
            TokenKind::Match => self.parse_match_expression(),
            TokenKind::Loop => self.parse_loop_expression(),
            TokenKind::Unsafe => {
                self.advance();
                self.parse_block()
                    .map(|block| Expression::Unsafe(Box::new(block)))
            }
            TokenKind::LeftBrace => self
                .parse_block()
                .map(|block| Expression::Block(Box::new(block))),
            TokenKind::LeftBracket => self.parse_array_expression(),
            TokenKind::LeftParen => {
                let start = self.advance();
                if let Some(end) = self.take(&TokenKind::RightParen) {
                    return Some(Expression::Unit(Span::new(start.span.start, end.span.end)));
                }
                let first = self.parse_expression()?;
                if self.take(&TokenKind::Comma).is_some() {
                    let mut elements = vec![first];
                    while !self.at(&TokenKind::RightParen) {
                        elements.push(self.parse_expression()?);
                        if self.take(&TokenKind::Comma).is_none() {
                            break;
                        }
                    }
                    let end =
                        self.expect_symbol(&TokenKind::RightParen, "`)` after tuple elements")?;
                    Some(Expression::Tuple(TupleExpression {
                        elements,
                        span: Span::new(start.span.start, end.span.end),
                    }))
                } else {
                    self.expect_symbol(&TokenKind::RightParen, "`)` after grouped expression")?;
                    Some(first)
                }
            }
            _ => {
                self.diagnostics.push(
                    Diagnostic::error("E1003", "expected an expression", token.span)
                        .with_help("insert a literal, path, call, block, or `if` expression"),
                );
                None
            }
        }
    }

    fn parse_pack_expansion_expression(&mut self) -> Option<Expression> {
        let start = self.advance();
        let pack = self.expect_identifier("type pack name after `...`")?;
        self.expect_symbol(&TokenKind::FatArrow, "`=>` after the type pack name")?;
        let template = self.parse_expression()?;
        let span = Span::new(start.span.start, template.span().end);
        Some(Expression::PackExpansion(Box::new(
            PackExpansionExpression {
                pack,
                template,
                span,
            },
        )))
    }

    fn parse_formatted_string(
        &mut self,
        fragments: Vec<LexicalFormattedFragment>,
        span: Span,
    ) -> Option<Expression> {
        let mut parsed = Vec::with_capacity(fragments.len());
        for fragment in fragments {
            match fragment {
                LexicalFormattedFragment::Text { value, span } => {
                    parsed.push(FormattedStringFragment::Text(StringLiteral { value, span }));
                }
                LexicalFormattedFragment::Expression { tokens, style, .. } => {
                    match Parser::new(&tokens).parse_complete_expression() {
                        Ok(expression) => {
                            parsed.push(match style {
                                FormattingStyle::Display => {
                                    FormattedStringFragment::Display(expression)
                                }
                                FormattingStyle::Debug => {
                                    FormattedStringFragment::Debug(expression)
                                }
                            });
                        }
                        Err(diagnostics) => {
                            self.diagnostics.extend(diagnostics);
                            return None;
                        }
                    }
                }
            }
        }
        Some(Expression::FormattedString(FormattedStringExpression {
            fragments: parsed,
            span,
        }))
    }

    fn parse_match_expression(&mut self) -> Option<Expression> {
        let start = self.advance();
        let scrutinee = self.parse_condition_expression()?;
        self.expect_symbol(&TokenKind::LeftBrace, "`{` before match arms")?;
        let mut arms = Vec::new();
        while !self.at(&TokenKind::RightBrace) && !self.at(&TokenKind::Eof) {
            let arm_start = self.current().span.start;
            let pattern = self.parse_pattern()?;
            let guard = if self.take(&TokenKind::If).is_some() {
                Some(self.parse_expression()?)
            } else {
                None
            };
            self.expect_symbol(&TokenKind::FatArrow, "`=>` after match pattern")?;
            let body = self.parse_expression()?;
            let end = body.span().end;
            self.take(&TokenKind::Comma);
            arms.push(MatchArm {
                pattern,
                guard,
                body,
                span: Span::new(arm_start, end),
            });
        }
        let end = self.expect_symbol(&TokenKind::RightBrace, "`}` after match arms")?;
        Some(Expression::Match(Box::new(MatchExpression {
            scrutinee,
            arms,
            span: Span::new(start.span.start, end.span.end),
        })))
    }

    fn parse_loop_expression(&mut self) -> Option<Expression> {
        let start = self.advance();
        let body = self.parse_block()?;
        let span = Span::new(start.span.start, body.span.end);
        Some(Expression::Loop(Box::new(LoopExpression { body, span })))
    }

    fn parse_pattern(&mut self) -> Option<Pattern> {
        self.with_nesting(Self::parse_pattern_inner)
    }

    fn parse_pattern_inner(&mut self) -> Option<Pattern> {
        let start = self.current().span.start;
        let mutable = self.take(&TokenKind::Mut).is_some();
        let negative = self.take(&TokenKind::Minus).is_some();
        let token = self.current().clone();
        match token.kind {
            TokenKind::Identifier(ref name) if name == "_" && !mutable && !negative => {
                self.advance();
                Some(Pattern::Wildcard(token.span))
            }
            TokenKind::Identifier(_) if !negative => self.parse_identifier_pattern(start, mutable),
            TokenKind::Integer(ref spelling) if !mutable => {
                self.advance();
                let Expression::Integer(literal) =
                    self.parse_integer_literal(spelling, token.span)?
                else {
                    unreachable!("integer pattern parsing produced a non-integer expression");
                };
                Some(Pattern::Integer {
                    value: literal.value,
                    negative,
                    span: Span::new(start, literal.span.end),
                })
            }
            TokenKind::Float(ref spelling) if !mutable => {
                self.advance();
                let Expression::Float(literal) = self.parse_float_literal(spelling, token.span)?
                else {
                    unreachable!("float pattern parsing produced a non-float expression");
                };
                Some(Pattern::Float {
                    bits: literal.bits,
                    negative,
                    span: Span::new(start, literal.span.end),
                })
            }
            TokenKind::Character(value) if !mutable && !negative => {
                self.advance();
                Some(Pattern::Character(CharacterLiteral {
                    value,
                    span: token.span,
                }))
            }
            TokenKind::True | TokenKind::False if !mutable && !negative => {
                self.advance();
                Some(Pattern::Boolean(BooleanLiteral {
                    value: matches!(token.kind, TokenKind::True),
                    span: token.span,
                }))
            }
            TokenKind::LeftParen if !mutable && !negative => {
                self.advance();
                let elements = self.parse_pattern_list(&TokenKind::RightParen)?;
                let end = self.expect_symbol(&TokenKind::RightParen, "`)` after tuple pattern")?;
                Some(Pattern::Tuple {
                    elements,
                    span: Span::new(start, end.span.end),
                })
            }
            _ => {
                self.diagnostics.push(
                    Diagnostic::error("E1014", "expected a pattern", token.span)
                        .with_help("use `_`, a binding, a literal, tuple, or enum pattern"),
                );
                None
            }
        }
    }

    fn parse_identifier_pattern(&mut self, start: usize, mutable: bool) -> Option<Pattern> {
        let path = self.parse_path()?;
        if self.take(&TokenKind::LeftParen).is_some() {
            if mutable {
                return self.invalid_pattern_modifier(start, path.span.end);
            }
            let fields = self.parse_pattern_list(&TokenKind::RightParen)?;
            let end =
                self.expect_symbol(&TokenKind::RightParen, "`)` after enum pattern fields")?;
            Some(Pattern::EnumTuple {
                path,
                fields,
                span: Span::new(start, end.span.end),
            })
        } else if self.take(&TokenKind::LeftBrace).is_some() {
            if mutable {
                return self.invalid_pattern_modifier(start, path.span.end);
            }
            let fields = self.parse_named_pattern_fields()?;
            let end =
                self.expect_symbol(&TokenKind::RightBrace, "`}` after enum pattern fields")?;
            Some(Pattern::EnumStruct {
                path,
                fields,
                span: Span::new(start, end.span.end),
            })
        } else if path.segments.len() == 1 {
            Some(Pattern::Identifier {
                mutable,
                name: path.segments[0].clone(),
                span: Span::new(start, path.span.end),
            })
        } else if mutable {
            self.invalid_pattern_modifier(start, path.span.end)
        } else {
            Some(Pattern::Path(path))
        }
    }

    fn parse_named_pattern_fields(&mut self) -> Option<Vec<PatternField>> {
        let mut fields = Vec::new();
        while !self.at(&TokenKind::RightBrace) && !self.at(&TokenKind::Eof) {
            let start = self.current().span.start;
            let name = self.expect_identifier("enum pattern field name")?;
            let pattern = if self.take(&TokenKind::Colon).is_some() {
                self.parse_pattern()?
            } else {
                Pattern::Identifier {
                    mutable: false,
                    name: name.clone(),
                    span: name.span,
                }
            };
            fields.push(PatternField {
                name,
                span: Span::new(start, pattern.span().end),
                pattern,
            });
            if self.take(&TokenKind::Comma).is_none() {
                break;
            }
        }
        Some(fields)
    }

    fn parse_pattern_list(&mut self, end: &TokenKind) -> Option<Vec<Pattern>> {
        let mut patterns = Vec::new();
        if self.at(end) {
            return Some(patterns);
        }
        loop {
            patterns.push(self.parse_pattern()?);
            if self.take(&TokenKind::Comma).is_none() || self.at(end) {
                break;
            }
        }
        Some(patterns)
    }

    fn invalid_pattern_modifier<T>(&mut self, start: usize, end: usize) -> Option<T> {
        self.diagnostics.push(
            Diagnostic::error(
                "E1014",
                "`mut` can only modify a binding pattern",
                Span::new(start, end),
            )
            .with_help("remove `mut` from this variant path"),
        );
        None
    }

    fn parse_array_expression(&mut self) -> Option<Expression> {
        let start = self.advance();
        let kind = if self.at(&TokenKind::RightBracket) {
            ArrayExpressionKind::List(Vec::new())
        } else {
            let first = self.parse_expression()?;
            if self.take(&TokenKind::Semicolon).is_some() {
                let length = self.parse_expression()?;
                ArrayExpressionKind::Repeat {
                    value: Box::new(first),
                    length: Box::new(length),
                }
            } else {
                let mut elements = vec![first];
                while self.take(&TokenKind::Comma).is_some() && !self.at(&TokenKind::RightBracket) {
                    elements.push(self.parse_expression()?);
                }
                ArrayExpressionKind::List(elements)
            }
        };
        let end = self.expect_symbol(&TokenKind::RightBracket, "`]` after array elements")?;
        Some(Expression::Array(ArrayExpression {
            kind,
            span: Span::new(start.span.start, end.span.end),
        }))
    }

    fn parse_struct_expression(&mut self, path: Path) -> Option<Expression> {
        let start = path.span.start;
        self.expect_symbol(&TokenKind::LeftBrace, "`{` before field initializers")?;
        let mut fields = Vec::new();
        while !self.at(&TokenKind::RightBrace) && !self.at(&TokenKind::Eof) {
            let name = self.expect_identifier("field initializer name")?;
            let field_start = name.span.start;
            self.expect_symbol(&TokenKind::Colon, "`:` after the field name")?;
            let value = self.parse_expression()?;
            let span = Span::new(field_start, value.span().end);
            fields.push(FieldInitializer { name, value, span });
            if self.take(&TokenKind::Comma).is_none() {
                break;
            }
        }
        let end = self.expect_symbol(&TokenKind::RightBrace, "`}` after field initializers")?;
        Some(Expression::Struct(StructExpression {
            path,
            fields,
            span: Span::new(start, end.span.end),
        }))
    }

    fn parse_integer_literal(&mut self, spelling: &str, span: Span) -> Option<Expression> {
        let (radix, digits) = integer_literal_parts(spelling);
        let normalized = digits.replace('_', "");
        if let Ok(value) = u128::from_str_radix(&normalized, radix) {
            Some(Expression::Integer(IntegerLiteral { value, span }))
        } else {
            self.diagnostics.push(Diagnostic::error(
                "E1004",
                format!("integer literal `{spelling}` is too large"),
                span,
            ));
            None
        }
    }

    fn parse_float_literal(&mut self, spelling: &str, span: Span) -> Option<Expression> {
        let normalized = spelling.replace('_', "");
        if let Ok(value) = normalized.parse::<f64>()
            && value.is_finite()
        {
            Some(Expression::Float(FloatLiteral {
                bits: value.to_bits(),
                span,
            }))
        } else {
            self.diagnostics.push(Diagnostic::error(
                "E1004",
                format!("floating-point literal `{spelling}` is out of range"),
                span,
            ));
            None
        }
    }

    fn parse_if_expression(&mut self) -> Option<Expression> {
        let start = self.advance();
        let condition = self.parse_condition_expression()?;
        let then_branch = self.parse_block()?;
        let else_branch = if self.take(&TokenKind::Else).is_some() {
            if self.at(&TokenKind::If) {
                Some(self.parse_if_expression()?)
            } else if self.at(&TokenKind::LeftBrace) {
                Some(Expression::Block(Box::new(self.parse_block()?)))
            } else {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E1012",
                        "expected `if` or a block after `else`",
                        self.current().span,
                    )
                    .with_help("use `else { ... }` or `else if ...`"),
                );
                return None;
            }
        } else {
            None
        };
        let end = else_branch
            .as_ref()
            .map_or(then_branch.span.end, |expression| expression.span().end);

        Some(Expression::If(Box::new(IfExpression {
            condition,
            then_branch,
            else_branch,
            span: Span::new(start.span.start, end),
        })))
    }

    fn expect_identifier(&mut self, role: &str) -> Option<Identifier> {
        let token = self.current().clone();
        let TokenKind::Identifier(name) = token.kind else {
            self.diagnostics.push(Diagnostic::error(
                "E1005",
                format!("expected {role}"),
                token.span,
            ));
            return None;
        };

        self.cursor += 1;
        Some(Identifier {
            name,
            span: token.span,
        })
    }

    fn expect_callable_name(&mut self, role: &str) -> Option<Identifier> {
        if self.at(&TokenKind::From) {
            let token = self.advance();
            return Some(Identifier {
                name: "from".to_owned(),
                span: token.span,
            });
        }
        self.expect_identifier(role)
    }

    fn expect_symbol(&mut self, kind: &TokenKind, expected: &str) -> Option<Token> {
        self.expect(
            kind,
            "E1006",
            format!("expected {expected}"),
            format!("insert {expected}"),
        )
    }

    fn expect_type_greater(&mut self) -> Option<()> {
        if self.pending_type_greater != 0 {
            self.pending_type_greater -= 1;
            return Some(());
        }
        if self.take(&TokenKind::Greater).is_some() {
            return Some(());
        }
        if self.take(&TokenKind::RightShift).is_some() {
            self.pending_type_greater = 1;
            return Some(());
        }
        self.diagnostics.push(
            Diagnostic::error(
                "E1006",
                "expected `>` after generic type arguments",
                self.current().span,
            )
            .with_help("close the generic argument list with `>`"),
        );
        None
    }

    fn previous_type_end(&self, fallback: usize) -> usize {
        if self.pending_type_greater != 0 {
            self.tokens[self.cursor.saturating_sub(1)].span.end
        } else {
            self.tokens
                .get(self.cursor.saturating_sub(1))
                .map_or(fallback, |token| token.span.end)
        }
    }

    fn expect(
        &mut self,
        kind: &TokenKind,
        code: &'static str,
        message: impl Into<String>,
        help: impl Into<String>,
    ) -> Option<Token> {
        if self.at(kind) {
            Some(self.advance())
        } else {
            self.diagnostics
                .push(Diagnostic::error(code, message, self.current().span).with_help(help));
            None
        }
    }

    fn take(&mut self, kind: &TokenKind) -> Option<Token> {
        self.at(kind).then(|| self.advance())
    }

    fn at(&self, kind: &TokenKind) -> bool {
        self.at_offset(0, kind)
    }

    fn at_offset(&self, offset: usize, kind: &TokenKind) -> bool {
        discriminant(&self.token_at(offset).kind) == discriminant(kind)
    }

    fn current(&self) -> &Token {
        self.token_at(0)
    }

    fn token_at(&self, offset: usize) -> &Token {
        &self.tokens[(self.cursor + offset).min(self.tokens.len().saturating_sub(1))]
    }

    fn advance(&mut self) -> Token {
        let token = self.current().clone();
        if !self.at(&TokenKind::Eof) {
            self.cursor += 1;
        }
        token
    }

    fn with_nesting<T>(&mut self, parse: fn(&mut Self) -> Option<T>) -> Option<T> {
        if self.nesting_depth >= MAX_NESTING_DEPTH {
            self.diagnostics.push(
                Diagnostic::error(
                    "E1022",
                    "syntax nesting limit exceeded",
                    self.current().span,
                )
                .with_help("reduce the number of nested expressions, types, or patterns"),
            );
            return None;
        }
        self.nesting_depth += 1;
        let result = parse(self);
        self.nesting_depth -= 1;
        result
    }

    fn synchronize_statement(&mut self) {
        while !self.at(&TokenKind::Eof) && !self.at(&TokenKind::RightBrace) {
            if self.take(&TokenKind::Semicolon).is_some() {
                return;
            }
            self.advance();
        }
    }

    fn synchronize_top_level(&mut self) {
        while !self.at(&TokenKind::Eof)
            && !self.at(&TokenKind::Fn)
            && !self.at(&TokenKind::Struct)
            && !self.at(&TokenKind::Enum)
            && !self.at(&TokenKind::From)
            && !self.at(&TokenKind::Import)
            && !self.at(&TokenKind::Const)
            && !self.at(&TokenKind::Static)
            && !self.at(&TokenKind::Pub)
        {
            self.advance();
        }
    }
}

fn integer_literal_parts(spelling: &str) -> (u32, &str) {
    if let Some(digits) = spelling
        .strip_prefix("0b")
        .or_else(|| spelling.strip_prefix("0B"))
    {
        (2, digits)
    } else if let Some(digits) = spelling
        .strip_prefix("0o")
        .or_else(|| spelling.strip_prefix("0O"))
    {
        (8, digits)
    } else if let Some(digits) = spelling
        .strip_prefix("0x")
        .or_else(|| spelling.strip_prefix("0X"))
    {
        (16, digits)
    } else {
        (10, spelling)
    }
}

fn binary_expression(operator: BinaryOperator, left: Expression, right: Expression) -> Expression {
    let span = Span::new(left.span().start, right.span().end);
    Expression::Binary(Box::new(BinaryExpression {
        operator,
        left,
        right,
        span,
    }))
}

#[cfg(test)]
mod tests {
    use reimer_ast::{
        ArrayExpressionKind, AssignmentOperator, BinaryOperator, Expression,
        FormattedStringFragment, GenericArgument, GenericParameter, ImportKind, Item, Statement,
        TypeName, TypeNameKind,
    };
    use reimer_lexer::lex;

    use super::{MAX_NESTING_DEPTH, parse};

    #[test]
    fn parse_should_build_m0_program() {
        let tokens = lex("fn main() -> i32 { return 42; }").expect("fixture should lex");

        let program = parse(&tokens).expect("fixture should parse");

        let Item::Function(function) = &program.items[0] else {
            panic!("expected function");
        };
        let Statement::Return(return_statement) = &function.body.statements[0] else {
            panic!("expected return");
        };
        assert!(matches!(
            return_statement.value,
            Some(Expression::Integer(integer)) if integer.value == 42
        ));
    }

    #[test]
    fn parse_should_make_progress_after_an_invalid_public_declaration() {
        let tokens = lex("pub unsafe fn invalid() {}").expect("fixture should lex");

        let diagnostics = parse(&tokens).expect_err("unsupported modifier should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E1001")
        );
    }

    #[test]
    fn parse_should_reject_excessive_syntax_nesting() {
        let depth = MAX_NESTING_DEPTH + 1;
        let pattern = format!("{}value{}", "(".repeat(depth), ",)".repeat(depth));
        let sources = [
            format!("fn main() {{ {}true; }}", "!".repeat(depth)),
            format!("type Deep = {}i32;", "& ".repeat(depth)),
            format!("fn main(value: i32) -> i32 {{ match value {{ {pattern} => 0 }} }}"),
        ];

        for source in sources {
            let tokens = lex(&source).expect("nesting fixture should lex");
            let diagnostics = parse(&tokens).expect_err("excessive nesting should fail safely");

            assert!(
                diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == "E1022"),
                "fixture should report the nesting limit: {source}"
            );
        }
    }

    #[test]
    fn parse_should_build_formatted_string_expressions() {
        let tokens =
            lex("fn message(name: str) { f\"hello {name}\"; }").expect("fixture should lex");

        let program = parse(&tokens).expect("fixture should parse");
        let Item::Function(function) = &program.items[0] else {
            panic!("expected function");
        };
        let Statement::Expression(statement) = &function.body.statements[0] else {
            panic!("expected expression statement");
        };
        let Expression::FormattedString(formatted) = &statement.expression else {
            panic!("expected formatted string");
        };

        assert!(matches!(
            &formatted.fragments[0],
            FormattedStringFragment::Text(literal) if literal.value == "hello "
        ));
        assert!(matches!(
            &formatted.fragments[1],
            FormattedStringFragment::Display(Expression::Path(path))
                if path.display() == "name"
        ));
    }

    #[test]
    fn parse_should_preserve_debug_interpolation_style() {
        let tokens =
            lex("fn main() { message.push_format(f\"{player:?}\"); }").expect("fixture should lex");
        let program = parse(&tokens).expect("fixture should parse");
        let Item::Function(function) = &program.items[0] else {
            panic!("fixture should contain a function");
        };
        let Statement::Expression(statement) = &function.body.statements[0] else {
            panic!("function should contain an expression statement");
        };
        let Expression::Call(call) = &statement.expression else {
            panic!("statement should be a call");
        };
        let Expression::FormattedString(formatted) = &call.arguments[0] else {
            panic!("call should contain a formatted string");
        };

        assert!(matches!(
            &formatted.fragments[0],
            FormattedStringFragment::Debug(Expression::Path(path))
                if path.display() == "player"
        ));
    }

    #[test]
    fn parse_should_build_public_type_alias() {
        let tokens = lex("pub type Index = usize;").expect("fixture should lex");

        let program = parse(&tokens).expect("fixture should parse");
        let Item::TypeAlias(declaration) = &program.items[0] else {
            panic!("expected type alias");
        };

        assert!(declaration.is_public);
        assert_eq!(declaration.name.name, "Index");
        assert!(matches!(declaration.target.kind, TypeNameKind::Path(_)));
    }

    #[test]
    fn parse_should_respect_arithmetic_precedence() {
        let tokens = lex("fn main() -> i32 { return 1 + 2 * 3; }").expect("fixture should lex");

        let program = parse(&tokens).expect("fixture should parse");
        let Item::Function(function) = &program.items[0] else {
            panic!("expected function");
        };
        let Statement::Return(return_statement) = &function.body.statements[0] else {
            panic!("expected return");
        };

        assert!(matches!(
            return_statement.value,
            Some(Expression::Binary(ref expression))
                if expression.operator == BinaryOperator::Add
        ));
    }

    #[test]
    fn parse_should_build_qualified_call_binding() {
        let tokens =
            lex("fn main() -> i32 { let value = x::y::z(); return 0; }").expect("fixture lexes");

        let program = parse(&tokens).expect("fixture should parse");
        let Item::Function(function) = &program.items[0] else {
            panic!("expected function");
        };
        let Statement::Let(binding) = &function.body.statements[0] else {
            panic!("expected binding");
        };

        assert!(matches!(binding.initializer, Expression::Call(_)));
    }

    #[test]
    fn parse_should_build_explicit_generic_function_value() {
        let tokens = lex("fn identity<T>(value: T) -> T { value }
             fn main() -> i32 {
                 let callback: fn(i32) -> i32 = identity::<i32>;
                 callback(42)
             }")
        .expect("fixture should lex");

        let program = parse(&tokens).expect("fixture should parse");
        let Item::Function(function) = &program.items[1] else {
            panic!("expected main function");
        };
        let Statement::Let(binding) = &function.body.statements[0] else {
            panic!("expected callback binding");
        };
        let Expression::GenericFunction(function) = &binding.initializer else {
            panic!("expected specialized generic function value");
        };

        assert_eq!(function.path.display(), "identity");
        assert_eq!(function.generic_arguments.len(), 1);
    }

    #[test]
    fn parse_should_lower_turbofish_calls_to_direct_paths() {
        let tokens = lex("fn identity<T>(value: T) -> T { value }
             fn main() -> i32 { identity::<i32>(42) }")
        .expect("fixture should lex");

        let program = parse(&tokens).expect("fixture should parse");
        let Item::Function(function) = &program.items[1] else {
            panic!("expected main function");
        };
        let Some(Expression::Call(call)) = function.body.tail.as_deref() else {
            panic!("expected direct call tail");
        };

        assert!(matches!(
            &call.callee,
            Expression::Path(path)
                if path.display() == "identity"
                    && call.has_explicit_generic_arguments
                    && call.generic_arguments.len() == 1
        ));
    }

    #[test]
    fn parse_should_preserve_explicit_empty_generic_call_lists() {
        let tokens = lex("fn ready<...Types>() -> bool { true }
             fn main() -> bool { ready::<>() }")
        .expect("fixture should lex");

        let program = parse(&tokens).expect("fixture should parse");
        let Item::Function(function) = &program.items[1] else {
            panic!("expected main function");
        };
        let Some(Expression::Call(call)) = function.body.tail.as_deref() else {
            panic!("expected direct call tail");
        };

        assert!(call.has_explicit_generic_arguments && call.generic_arguments.is_empty());
    }

    #[test]
    fn parse_should_build_if_tail_and_while_loop() {
        let source = "fn main() -> i32 {
            let mut value = 0;
            while value < 3 { value += 1; }
            if value == 3 { 42 } else { 0 }
        }";
        let tokens = lex(source).expect("fixture should lex");

        let program = parse(&tokens).expect("fixture should parse");
        let Item::Function(function) = &program.items[0] else {
            panic!("expected function");
        };

        assert!(matches!(
            function.body.tail.as_deref(),
            Some(Expression::If(_))
        ));
    }

    #[test]
    fn parse_should_report_missing_semicolon() {
        let tokens = lex("fn main() -> i32 { return 42 }").expect("fixture should lex");

        let diagnostics = parse(&tokens).expect_err("fixture should fail");

        assert_eq!(diagnostics[0].code, "E1006");
    }

    #[test]
    fn parse_should_build_selective_double_colon_import() {
        let tokens = lex("from game::math import Vec3 as Position;").expect("fixture should lex");

        let program = parse(&tokens).expect("fixture should parse");

        let Item::Import(import) = &program.items[0] else {
            panic!("expected import");
        };
        let ImportKind::Symbols { module, names } = &import.kind else {
            panic!("expected selective import");
        };
        assert_eq!(module.display(), "game::math");
        assert_eq!(
            names[0].alias.as_ref().map(|name| name.name.as_str()),
            Some("Position")
        );
    }

    #[test]
    fn parse_should_build_public_module_import() {
        let tokens = lex("pub import engine::render as graphics;").expect("fixture should lex");

        let program = parse(&tokens).expect("fixture should parse");

        let Item::Import(import) = &program.items[0] else {
            panic!("expected import");
        };
        assert!(import.is_public);
    }

    #[test]
    fn parse_should_build_public_function_with_parameters() {
        let tokens = lex("pub fn add(left: i32, right: i32) -> i32 { left + right }")
            .expect("fixture should lex");

        let program = parse(&tokens).expect("fixture should parse");
        let Item::Function(function) = &program.items[0] else {
            panic!("expected function");
        };

        assert_eq!(function.parameters.len(), 2);
    }

    #[test]
    fn parse_should_build_scalar_literals_and_casts() {
        let tokens = lex("fn main() -> i32 { let ratio: f32 = 1.5; 'A' as u32 as i32 }")
            .expect("fixture should lex");

        let program = parse(&tokens).expect("fixture should parse");
        let Item::Function(function) = &program.items[0] else {
            panic!("expected function");
        };
        let Statement::Let(binding) = &function.body.statements[0] else {
            panic!("expected binding");
        };
        assert!(matches!(binding.initializer, Expression::Float(_)));
        let Some(Expression::Cast(outer)) = function.body.tail.as_deref() else {
            panic!("expected cast tail");
        };
        assert!(matches!(outer.value, Expression::Cast(_)));
        assert!(matches!(outer.target.kind, TypeNameKind::Path(_)));
    }

    #[test]
    fn parse_should_normalize_integer_bases_and_separators() {
        let source = "fn main() -> i32 { let values = (0xFF, 0b1010, 0o755, 1_000_000); 0 }";
        let tokens = lex(source).expect("fixture should lex");

        let program = parse(&tokens).expect("fixture should parse");
        let Item::Function(function) = &program.items[0] else {
            panic!("expected function");
        };
        let Statement::Let(binding) = &function.body.statements[0] else {
            panic!("expected binding");
        };
        let Expression::Tuple(tuple) = &binding.initializer else {
            panic!("expected tuple");
        };
        let values = tuple
            .elements
            .iter()
            .map(|element| match element {
                Expression::Integer(literal) => literal.value,
                _ => panic!("expected integer literal"),
            })
            .collect::<Vec<_>>();

        assert_eq!(values, [255, 10, 493, 1_000_000]);
    }

    #[test]
    fn parse_should_preserve_c_string_literal_kind() {
        let tokens =
            lex(r#"fn main() { let title: cstr = c"Reimer"; }"#).expect("fixture should lex");

        let program = parse(&tokens).expect("fixture should parse");
        let Item::Function(function) = &program.items[0] else {
            panic!("expected function");
        };
        let Statement::Let(binding) = &function.body.statements[0] else {
            panic!("expected binding");
        };

        assert!(matches!(
            &binding.initializer,
            Expression::CString(literal) if literal.value == "Reimer"
        ));
    }

    #[test]
    fn parse_should_respect_shift_and_bitwise_precedence() {
        let tokens =
            lex("fn main() -> i32 { 1 | 2 ^ 3 & 4 << 1 + 1 }").expect("fixture should lex");

        let program = parse(&tokens).expect("fixture should parse");
        let Item::Function(function) = &program.items[0] else {
            panic!("expected function");
        };
        assert!(matches!(
            function.body.tail.as_deref(),
            Some(Expression::Binary(expression))
                if expression.operator == BinaryOperator::BitOr
        ));
    }

    #[test]
    fn parse_should_build_bitwise_compound_assignment() {
        let tokens =
            lex("fn main() { let mut value = 1; value <<= 2; }").expect("fixture should lex");

        let program = parse(&tokens).expect("fixture should parse");
        let Item::Function(function) = &program.items[0] else {
            panic!("expected function");
        };
        let Statement::Expression(statement) = &function.body.statements[1] else {
            panic!("expected expression statement");
        };
        assert!(matches!(
            statement.expression,
            Expression::Assignment(ref expression)
                if expression.operator == AssignmentOperator::ShiftLeft
        ));
    }

    #[test]
    fn parse_should_build_composite_declarations_types_and_expressions() {
        let source = "pub struct Pair { pub left: i32, right: i32 }
            enum Value { Empty, Pair(i32, bool), Named { value: i32 } }
            fn main() -> i32 {
                let pair: Pair = Pair { left: 20, right: 22 };
                let values: [i32; 2] = [pair.left, pair.right];
                let tuple: (i32, bool) = (values[0], true);
                tuple.0
            }";
        let tokens = lex(source).expect("fixture should lex");

        let program = parse(&tokens).expect("fixture should parse");

        assert!(matches!(program.items[0], Item::Struct(_)));
        assert!(matches!(program.items[1], Item::Enum(_)));
        let Item::Function(function) = &program.items[2] else {
            panic!("expected function");
        };
        let Statement::Let(array) = &function.body.statements[1] else {
            panic!("expected array binding");
        };
        assert!(matches!(
            array.ty.as_ref().map(|ty| &ty.kind),
            Some(TypeNameKind::Array { .. })
        ));
        assert!(matches!(
            function.body.tail.as_deref(),
            Some(Expression::Field(_))
        ));
    }

    #[test]
    fn parse_should_distinguish_repeated_array_initializers() {
        let tokens =
            lex("fn main() { let values: [i32; 4] = [0; 4]; }").expect("fixture should lex");

        let program = parse(&tokens).expect("fixture should parse");
        let Item::Function(function) = &program.items[0] else {
            panic!("expected function");
        };
        let Statement::Let(binding) = &function.body.statements[0] else {
            panic!("expected array binding");
        };
        let Expression::Array(array) = &binding.initializer else {
            panic!("expected array expression");
        };
        assert!(matches!(
            array.kind,
            ArrayExpressionKind::Repeat {
                ref value,
                ref length,
            } if matches!(value.as_ref(), Expression::Integer(value) if value.value == 0)
                && matches!(length.as_ref(), Expression::Integer(length) if length.value == 4)
        ));
    }

    #[test]
    fn parse_should_build_match_loop_for_and_patterns() {
        let source = "enum Value { Empty, Pair(i32, i32) }
            fn main() -> i32 {
                let values = [20, 22];
                for mut value in values { value += 1; }
                loop {
                    break match Value::Pair(20, 22) {
                        Value::Empty => 0,
                        Value::Pair(left, right) if right == 22 => left + right,
                        Value::Pair(_, _) => 0,
                    };
                }
            }";
        let tokens = lex(source).expect("fixture should lex");

        let program = parse(&tokens).expect("fixture should parse");
        let Item::Function(function) = &program.items[1] else {
            panic!("expected function");
        };

        assert!(matches!(function.body.statements[1], Statement::For(_)));
        assert!(matches!(
            function.body.tail.as_deref(),
            Some(Expression::Loop(_))
        ));
    }

    #[test]
    fn parse_should_build_nested_intrinsic_types_and_try_expression() {
        let source = "fn nested(value: Result<Option<i32>, i32>)
                -> Result<Option<i32>, i32> {
            value?
        }";
        let tokens = lex(source).expect("fixture should lex");

        let program = parse(&tokens).expect("fixture should parse");
        let Item::Function(function) = &program.items[0] else {
            panic!("expected function");
        };
        let Some(return_type) = &function.return_type else {
            panic!("expected return type");
        };
        let TypeNameKind::Generic { arguments, .. } = &return_type.kind else {
            panic!("expected generic result type");
        };

        assert_eq!(arguments.len(), 2);
        assert!(matches!(
            &arguments[0],
            GenericArgument::Type(TypeName {
                kind: TypeNameKind::Generic { arguments, .. },
                ..
            }) if arguments.len() == 1
        ));
        assert!(matches!(
            function.body.tail.as_deref(),
            Some(Expression::Try { .. })
        ));
    }

    #[test]
    fn parse_should_build_expression_and_block_defers() {
        let source = "fn cleanup() {
            defer release();
            defer { release(); }
        }";
        let tokens = lex(source).expect("fixture should lex");

        let program = parse(&tokens).expect("fixture should parse");
        let Item::Function(function) = &program.items[0] else {
            panic!("expected function");
        };

        assert!(matches!(function.body.statements[0], Statement::Defer(_)));
        assert!(matches!(function.body.statements[1], Statement::Defer(_)));
    }

    #[test]
    fn parse_should_build_a_c_abi_function_declaration() {
        let tokens =
            lex("extern \"C\" fn native_abs(value: i32) -> i32;").expect("fixture should lex");

        let program = parse(&tokens).expect("fixture should parse");

        assert!(matches!(
            program.items.as_slice(),
            [Item::ExternFunction(function)] if function.abi == "C"
        ));
    }

    #[test]
    fn parse_should_flatten_a_linked_extern_block() {
        let source = "@link(\"raylib\") extern \"C\" {
            fn InitWindow(width: i32, height: i32);
            pub fn WindowShouldClose() -> bool;
        }";
        let tokens = lex(source).expect("fixture should lex");

        let program = parse(&tokens).expect("fixture should parse");

        assert_eq!(program.items.len(), 2);
        assert!(matches!(
            &program.items[0],
            Item::ExternFunction(function)
                if function.link.as_deref() == Some("raylib") && !function.is_public
        ));
        assert!(matches!(
            &program.items[1],
            Item::ExternFunction(function)
                if function.link.as_deref() == Some("raylib") && function.is_public
        ));
    }

    #[test]
    fn parse_should_mark_a_c_representation_struct() {
        let tokens = lex("@repr(C) pub struct Vector2 { pub x: f32, pub y: f32 }")
            .expect("fixture should lex");

        let program = parse(&tokens).expect("fixture should parse");

        assert!(matches!(
            program.items.as_slice(),
            [Item::Struct(declaration)]
                if matches!(
                    declaration.attributes.as_slice(),
                    [reimer_ast::Attribute {
                        name,
                        arguments,
                        ..
                    }] if name.name == "repr"
                        && matches!(
                            arguments.as_slice(),
                            [reimer_ast::AttributeArgument::Identifier(value)]
                                if value.name == "C"
                        )
                )
        ));
    }

    #[test]
    fn parse_should_build_m10_attributes_constants_and_comptime_items() {
        let source = "
            @derive(Copy, Eq, Default)
            @align(16)
            struct Pair { left: i32, right: i32 }

            comptime fn factorial(value: usize) -> usize {
                if value <= 1 { 1 } else { value * factorial(value - 1) }
            }

            const TABLE_SIZE: usize = factorial(5);
            comptime { assert(TABLE_SIZE == 120); }

            fn main() -> i32 {
                let pair = Pair::default();
                size_of<Pair>() as i32
            }
        ";
        let tokens = lex(source).expect("M10 fixture should lex");

        let program = parse(&tokens).expect("M10 fixture should parse");

        assert!(matches!(
            program.items.as_slice(),
            [
                Item::Struct(_),
                Item::Function(function),
                Item::Constant(_),
                Item::Comptime(_),
                Item::Function(_)
            ] if function.is_comptime
        ));
    }

    #[test]
    fn parse_should_build_immutable_and_mutable_static_declarations() {
        let tokens = lex("pub static ANSWER: i32 = 42;
             static mut COUNTER: usize = 0;")
        .expect("fixture should lex");

        let program = parse(&tokens).expect("fixture should parse");

        assert!(matches!(
            program.items.as_slice(),
            [Item::Static(answer), Item::Static(counter)]
                if answer.is_public
                    && !answer.mutable
                    && answer.name.name == "ANSWER"
                    && !counter.is_public
                    && counter.mutable
                    && counter.name.name == "COUNTER"
        ));
    }

    #[test]
    fn parse_should_build_inherent_methods_and_self_receivers() {
        let source = "struct Counter { value: i32 }
            impl Counter {
                fn add(&mut self, amount: i32) { self.value += amount; }
                fn get(&self) -> i32 { self.value }
            }";
        let tokens = lex(source).expect("fixture should lex");

        let program = parse(&tokens).expect("fixture should parse");

        let Item::Impl(implementation) = &program.items[1] else {
            panic!("expected impl declaration");
        };
        assert_eq!(implementation.methods.len(), 2);
        assert!(matches!(
            implementation.methods[0].parameters[0].ty.kind,
            TypeNameKind::Reference { mutable: true, .. }
        ));
    }

    #[test]
    fn parse_should_build_generic_declarations_and_const_arguments() {
        let source = "
            struct Buffer<T: Copy, const N: usize> where T: Ordered {
                values: [T; N]
            }
            fn choose<T>(left: T, right: T) -> T where T: Ordered { left }
            fn accepts(value: Buffer<i32, 4>) {}
            fn main() -> i32 { 42 }";
        let tokens = lex(source).expect("fixture should lex");

        let program = parse(&tokens).expect("fixture should parse");

        let Item::Struct(buffer) = &program.items[0] else {
            panic!("expected generic struct");
        };
        assert_eq!(buffer.generic_parameters.len(), 2);
        assert_eq!(buffer.where_predicates.len(), 1);
        let Item::Function(accepts) = &program.items[2] else {
            panic!("expected accepts function");
        };
        assert!(matches!(
            &accepts.parameters[0].ty.kind,
            TypeNameKind::Generic { arguments, .. }
                if matches!(arguments.get(1), Some(GenericArgument::Const(_)))
        ));
    }

    #[test]
    fn parse_should_allow_an_empty_variadic_argument_list() {
        let source = "struct Registry<...Values> { values: (...Values) }
            type EmptyRegistry = Registry<>;";
        let tokens = lex(source).expect("fixture should lex");

        let program = parse(&tokens).expect("fixture should parse");

        let Item::TypeAlias(alias) = &program.items[1] else {
            panic!("expected type alias");
        };
        assert!(matches!(
            &alias.target.kind,
            TypeNameKind::Generic { arguments, .. } if arguments.is_empty()
        ));
    }

    #[test]
    fn parse_should_build_trait_and_trait_impl() {
        let source = "
            trait Measure {
                fn measure(&self) -> i32;
            }
            struct Counter { value: i32 }
            impl Measure for Counter {
                fn measure(&self) -> i32 { self.value }
            }";
        let tokens = lex(source).expect("fixture should lex");

        let program = parse(&tokens).expect("fixture should parse");

        assert!(matches!(
            &program.items[0],
            Item::Trait(declaration) if declaration.methods.len() == 1
        ));
        assert!(matches!(
            &program.items[2],
            Item::Impl(declaration) if declaration.trait_type.is_some()
        ));
    }

    #[test]
    fn parse_should_allow_from_as_an_associated_function_name() {
        let source = "struct Text {}
            impl Text {
                fn from(value: str) -> Text { Text {} }
            }
            fn main() -> i32 {
                let text = Text::from(\"typed\");
                42
            }";
        let tokens = lex(source).expect("fixture should lex");

        let program = parse(&tokens).expect("fixture should parse");

        let Item::Impl(implementation) = &program.items[1] else {
            panic!("expected impl declaration");
        };
        assert_eq!(implementation.methods[0].name.name, "from");
    }

    #[test]
    fn parse_should_split_nested_generic_closers_before_a_comma() {
        let source = "struct Entry<K, V> { key: K, value: V }
            struct Vec<T> { value: T }
            enum Result<T, E> { Ok(T), Err(E) }
            struct Error {}
            fn collect() -> Result<Vec<Entry<i32, i32>>, Error> {
                Result::Err(Error {})
            }";
        let tokens = lex(source).expect("fixture should lex");

        let program = parse(&tokens).expect("fixture should parse");

        let Item::Function(function) = &program.items[4] else {
            panic!("expected function declaration");
        };
        assert!(matches!(
            function.return_type.as_ref().map(|ty| &ty.kind),
            Some(TypeNameKind::Generic { arguments, .. }) if arguments.len() == 2
        ));
    }

    #[test]
    fn parse_should_build_function_pointer_types() {
        let source = "
            fn apply(callback: fn(i32, i32) -> i32) -> i32 {
                callback(20, 22)
            }";
        let tokens = lex(source).expect("fixture should lex");

        let program = parse(&tokens).expect("fixture should parse");

        let Item::Function(function) = &program.items[0] else {
            panic!("expected function declaration");
        };
        assert!(matches!(
            &function.parameters[0].ty.kind,
            TypeNameKind::Function {
                parameters,
                return_type,
            } if parameters.len() == 2
                && matches!(return_type.kind, TypeNameKind::Path(_))
        ));
    }

    #[test]
    fn parse_should_build_variadic_parameters_and_mapped_expansions() {
        let source = "
            struct Slot<T> { value: T }
            struct Registry<...Types> {
                stores: (...Types => Slot<Types>),
            }";
        let tokens = lex(source).expect("fixture should lex");

        let program = parse(&tokens).expect("fixture should parse");

        let Item::Struct(registry) = &program.items[1] else {
            panic!("expected registry declaration");
        };
        assert!(matches!(
            registry.generic_parameters.as_slice(),
            [GenericParameter::TypePack { name, .. }] if name.name == "Types"
        ));
        let TypeNameKind::Tuple(elements) = &registry.fields[0].ty.kind else {
            panic!("expected tuple storage");
        };
        assert!(matches!(
            elements.as_slice(),
            [TypeName {
                kind: TypeNameKind::PackExpansion {
                    pack,
                    template: Some(_),
                },
                ..
            }] if pack.name == "Types"
        ));
    }
}
