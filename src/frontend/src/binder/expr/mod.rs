// Copyright 2022 Singularity Data
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use itertools::zip_eq;
use risingwave_common::error::{ErrorCode, Result};
use risingwave_common::types::DataType;
use risingwave_sqlparser::ast::{
    BinaryOperator, DataType as AstDataType, DateTimeField, Expr, Query, TrimWhereField,
    UnaryOperator,
};

use crate::binder::Binder;
use crate::expr::{Expr as _, ExprImpl, ExprType, FunctionCall, SubqueryKind};

mod binary_op;
mod column;
mod function;
mod subquery;
mod value;

impl Binder {
    pub(super) fn bind_expr(&mut self, expr: Expr) -> Result<ExprImpl> {
        match expr {
            Expr::IsNull(expr) => Ok(ExprImpl::FunctionCall(Box::new(
                self.bind_is_operator(ExprType::IsNull, *expr)?,
            ))),
            Expr::IsNotNull(expr) => Ok(ExprImpl::FunctionCall(Box::new(
                self.bind_is_operator(ExprType::IsNotNull, *expr)?,
            ))),
            Expr::IsTrue(expr) => Ok(ExprImpl::FunctionCall(Box::new(
                self.bind_is_operator(ExprType::IsTrue, *expr)?,
            ))),
            Expr::IsNotTrue(expr) => Ok(ExprImpl::FunctionCall(Box::new(
                self.bind_is_operator(ExprType::IsNotTrue, *expr)?,
            ))),
            Expr::IsFalse(expr) => Ok(ExprImpl::FunctionCall(Box::new(
                self.bind_is_operator(ExprType::IsFalse, *expr)?,
            ))),
            Expr::IsNotFalse(expr) => Ok(ExprImpl::FunctionCall(Box::new(
                self.bind_is_operator(ExprType::IsNotFalse, *expr)?,
            ))),
            Expr::Case {
                operand,
                conditions,
                results,
                else_result,
            } => Ok(ExprImpl::FunctionCall(Box::new(self.bind_case(
                operand,
                conditions,
                results,
                else_result,
            )?))),
            Expr::Trim { expr, trim_where } => Ok(ExprImpl::FunctionCall(Box::new(
                self.bind_trim(*expr, trim_where)?,
            ))),
            Expr::Identifier(ident) => self.bind_column(&[ident]),
            Expr::CompoundIdentifier(idents) => self.bind_column(&idents),
            Expr::FieldIdentifier(field_expr, idents) => {
                Ok(self.bind_single_field_column(*field_expr, &idents)?)
            }
            Expr::Value(v) => Ok(ExprImpl::Literal(Box::new(self.bind_value(v)?))),
            Expr::BinaryOp { left, op, right } => Ok(ExprImpl::FunctionCall(Box::new(
                self.bind_binary_op(*left, op, *right)?,
            ))),
            Expr::UnaryOp { op, expr } => Ok(self.bind_unary_expr(op, *expr)?),
            Expr::Nested(expr) => self.bind_expr(*expr),
            Expr::Cast { expr, data_type } => self.bind_cast(*expr, data_type),
            Expr::Function(f) => Ok(self.bind_function(f)?),
            Expr::Subquery(q) => Ok(self.bind_subquery_expr(*q, SubqueryKind::Scalar)?),
            Expr::Exists(q) => Ok(self.bind_subquery_expr(*q, SubqueryKind::Existential)?),
            Expr::InSubquery {
                expr,
                subquery,
                negated,
            } => self.bind_in_subquery(*expr, *subquery, negated),
            Expr::TypedString { data_type, value } => {
                let s: ExprImpl = self.bind_string(value)?.into();
                s.cast_explicit(bind_data_type(&data_type)?)
            }
            Expr::Between {
                expr,
                negated,
                low,
                high,
            } => Ok(ExprImpl::FunctionCall(Box::new(
                self.bind_between(*expr, negated, *low, *high)?,
            ))),
            Expr::Extract { field, expr } => self.bind_extract(field, *expr),
            Expr::InList {
                expr,
                list,
                negated,
            } => self.bind_in_list(*expr, list, negated),
            _ => Err(ErrorCode::NotImplemented(
                format!("unsupported expression {:?}", expr),
                112.into(),
            )
            .into()),
        }
    }

    pub(super) fn bind_extract(&mut self, field: DateTimeField, expr: Expr) -> Result<ExprImpl> {
        let arg = self.bind_expr(expr)?;
        let arg_type = arg.return_type();
        Ok(FunctionCall::new(
            ExprType::Extract,
            vec![self.bind_string(field.to_string())?.into(), arg],
        )
        .map_err(|_| {
            ErrorCode::NotImplemented(
                format!(
                    "function extract({} from {:?}) doesn't exist",
                    field, arg_type
                ),
                112.into(),
            )
        })?
        .into())
    }

    pub(super) fn bind_in_list(
        &mut self,
        expr: Expr,
        list: Vec<Expr>,
        negated: bool,
    ) -> Result<ExprImpl> {
        let mut bound_expr_list = vec![self.bind_expr(expr)?];
        for elem in list {
            bound_expr_list.push(self.bind_expr(elem)?);
        }
        let in_expr = FunctionCall::new(ExprType::In, bound_expr_list)?;
        if negated {
            Ok(
                FunctionCall::new_unchecked(ExprType::Not, vec![in_expr.into()], DataType::Boolean)
                    .into(),
            )
        } else {
            Ok(in_expr.into())
        }
    }

    pub(super) fn bind_in_subquery(
        &mut self,
        expr: Expr,
        subquery: Query,
        negated: bool,
    ) -> Result<ExprImpl> {
        let bound_expr = self.bind_expr(expr)?;
        let bound_subquery = self.bind_subquery_expr(subquery, SubqueryKind::In(bound_expr))?;
        if negated {
            Ok(
                FunctionCall::new_unchecked(ExprType::Not, vec![bound_subquery], DataType::Boolean)
                    .into(),
            )
        } else {
            Ok(bound_subquery)
        }
    }

    pub(super) fn bind_unary_expr(&mut self, op: UnaryOperator, expr: Expr) -> Result<ExprImpl> {
        let func_type = match op {
            UnaryOperator::Not => ExprType::Not,
            UnaryOperator::Minus => ExprType::Neg,
            UnaryOperator::Plus => {
                return self.rewrite_positive(expr);
            }
            _ => {
                return Err(ErrorCode::NotImplemented(
                    format!("unsupported unary expression: {:?}", op),
                    112.into(),
                )
                .into())
            }
        };
        let expr = self.bind_expr(expr)?;
        FunctionCall::new(func_type, vec![expr]).map(|f| f.into())
    }

    /// Directly returns the expression itself if it is a positive number.
    fn rewrite_positive(&mut self, expr: Expr) -> Result<ExprImpl> {
        let expr = self.bind_expr(expr)?;
        let return_type = expr.return_type();
        if return_type.is_numeric() {
            return Ok(expr);
        }
        return Err(ErrorCode::InvalidInputSyntax(format!("+ {:?}", return_type)).into());
    }

    pub(super) fn bind_trim(
        &mut self,
        expr: Expr,
        // ([BOTH | LEADING | TRAILING], <expr>)
        trim_where: Option<(TrimWhereField, Box<Expr>)>,
    ) -> Result<FunctionCall> {
        let mut inputs = vec![self.bind_expr(expr)?];
        let func_type = match trim_where {
            Some(t) => {
                inputs.push(self.bind_expr(*t.1)?);
                match t.0 {
                    TrimWhereField::Both => ExprType::Trim,
                    TrimWhereField::Leading => ExprType::Ltrim,
                    TrimWhereField::Trailing => ExprType::Rtrim,
                }
            }
            None => ExprType::Trim,
        };
        FunctionCall::new(func_type, inputs)
    }

    pub(super) fn bind_concat_ws(
        &mut self,
        sep_expr: Expr,
        string_exprs: Vec<Expr>,
    ) -> Result<FunctionCall> {
        let mut inputs = Vec::new();
        inputs.push(self.bind_expr(sep_expr)?);
        for string_expr in string_exprs.iter() {
            inputs.push(self.bind_expr(string_expr.to_owned())?);
        }
        FunctionCall::new(ExprType::ConcatWs, inputs)
    }

    /// Bind `expr (not) between low and high`
    pub(super) fn bind_between(
        &mut self,
        expr: Expr,
        negated: bool,
        low: Expr,
        high: Expr,
    ) -> Result<FunctionCall> {
        let expr = self.bind_expr(expr)?;
        let low = self.bind_expr(low)?;
        let high = self.bind_expr(high)?;

        let func_call = if negated {
            // negated = true: expr < low or expr > high
            FunctionCall::new_unchecked(
                ExprType::Or,
                vec![
                    FunctionCall::new(ExprType::LessThan, vec![expr.clone(), low])?.into(),
                    FunctionCall::new(ExprType::GreaterThan, vec![expr, high])?.into(),
                ],
                DataType::Boolean,
            )
        } else {
            // negated = false: expr >= low and expr <= high
            FunctionCall::new_unchecked(
                ExprType::And,
                vec![
                    FunctionCall::new(ExprType::GreaterThanOrEqual, vec![expr.clone(), low])?
                        .into(),
                    FunctionCall::new(ExprType::LessThanOrEqual, vec![expr, high])?.into(),
                ],
                DataType::Boolean,
            )
        };

        Ok(func_call)
    }

    pub(super) fn bind_case(
        &mut self,
        operand: Option<Box<Expr>>,
        conditions: Vec<Expr>,
        results: Vec<Expr>,
        else_result: Option<Box<Expr>>,
    ) -> Result<FunctionCall> {
        let mut inputs = Vec::new();
        let results_expr: Vec<ExprImpl> = results
            .into_iter()
            .map(|expr| self.bind_expr(expr))
            .collect::<Result<_>>()?;
        let else_result_expr = else_result.map(|expr| self.bind_expr(*expr)).transpose()?;

        for (condition, result) in zip_eq(conditions, results_expr) {
            let condition = match operand {
                Some(ref t) => Expr::BinaryOp {
                    left: t.clone(),
                    op: BinaryOperator::Eq,
                    right: Box::new(condition),
                },
                None => condition,
            };
            inputs.push(self.bind_expr(condition)?);
            inputs.push(result);
        }
        if let Some(expr) = else_result_expr {
            inputs.push(expr);
        }
        FunctionCall::new(ExprType::Case, inputs)
    }

    pub(super) fn bind_is_operator(
        &mut self,
        func_type: ExprType,
        expr: Expr,
    ) -> Result<FunctionCall> {
        let expr = self.bind_expr(expr)?;
        FunctionCall::new(func_type, vec![expr])
    }

    pub(super) fn bind_cast(&mut self, expr: Expr, data_type: AstDataType) -> Result<ExprImpl> {
        self.bind_expr(expr)?
            .cast_explicit(bind_data_type(&data_type)?)
    }
}

pub fn bind_data_type(data_type: &AstDataType) -> Result<DataType> {
    let data_type = match data_type {
        AstDataType::Boolean => DataType::Boolean,
        AstDataType::SmallInt(None) => DataType::Int16,
        AstDataType::Int(None) => DataType::Int32,
        AstDataType::BigInt(None) => DataType::Int64,
        AstDataType::Real | AstDataType::Float(Some(1..=24)) => DataType::Float32,
        AstDataType::Double | AstDataType::Float(Some(25..=53) | None) => DataType::Float64,
        AstDataType::Decimal(None, None) => DataType::Decimal,
        AstDataType::Varchar(_) => DataType::Varchar,
        AstDataType::Date => DataType::Date,
        AstDataType::Time(false) => DataType::Time,
        AstDataType::Timestamp(false) => DataType::Timestamp,
        AstDataType::Timestamp(true) => DataType::Timestampz,
        AstDataType::Interval => DataType::Interval,
        AstDataType::Array(datatype) => DataType::List {
            datatype: Box::new(bind_data_type(datatype)?),
        },
        AstDataType::Char(..) => {
            return Err(ErrorCode::NotImplemented(
                "CHAR is not supported, please use VARCHAR instead\n".to_string(),
                None.into(),
            )
            .into())
        }
        _ => {
            return Err(ErrorCode::NotImplemented(
                format!("unsupported data type: {:?}", data_type),
                None.into(),
            )
            .into())
        }
    };
    Ok(data_type)
}
