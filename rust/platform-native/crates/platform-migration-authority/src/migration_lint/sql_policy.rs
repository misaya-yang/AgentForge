use std::collections::{BTreeMap, BTreeSet};

use super::grants::is_approved_role_prefix_grant_body;
use super::sql_lexer::{Token, TokenKind, ddl_words, split_statements, tokenize, words};
use super::sql_safety::{
    changes_authorization_identity, changes_search_path, column_default_may_rewrite,
    controls_transaction_boundary, has_phrase, index_name, index_target_relation,
    newly_created_table, plain_index_targets_new_empty_table, split_timeout_override_in_body,
    starts_constraint_kind, starts_with, timeout_override,
};
use super::{
    APPROVED_OWNER_PRECONDITION, ChangeSpec, Finding, MAX_LOCK_TIMEOUT_SECONDS,
    MAX_STATEMENT_TIMEOUT_SECONDS, NON_TRANSACTIONAL_TIMEOUT_RUNNER, Result,
    TRANSACTIONAL_TIMEOUT_RUNNER, TransactionMode, push_finding,
};

pub(super) fn lint_sql(sql: &str, change: &ChangeSpec) -> Result<(Vec<Finding>, &'static str)> {
    let statements = split_statements(sql)?;
    let reviewed029 = super::reviewed_schema::exact029(sql, change);
    let staged_not_null = super::reviewed_schema::staged_not_null(sql, change);
    let reviewed_partition_installer = super::reviewed_schema::partition_installer(sql, change);
    let mut findings = Vec::new();
    let mut concurrent_unique_indexes = BTreeSet::new();
    let mut pending_validation = Vec::new();
    let mut validated_constraints = BTreeMap::new();
    let mut has_expansion = false;
    let mut has_contraction = false;

    for (offset, statement) in statements.iter().enumerate() {
        let statement_index = offset + 1;
        let statement_words = ddl_words(statement);
        let allow_role_prefix_grants =
            super::reviewed_schema::grant_body(reviewed029, statement_index)
                || if starts_with(&statement_words, &["DO"]) && statement.len() == 2 {
                    match &statement[1].kind {
                        TokenKind::Body(body) => is_approved_role_prefix_grant_body(body, change)?,
                        _ => false,
                    }
                } else {
                    false
                };
        if let Some(code) = timeout_override(statement, &words(statement)) {
            push_finding(&mut findings, "blocked", code, statement_index);
        }
        if changes_authorization_identity(statement) {
            push_finding(
                &mut findings,
                "blocked",
                "migration_sql_cannot_change_authorization_identity",
                statement_index,
            );
        }
        if ((starts_with(&statement_words, &["SET"]) || starts_with(&statement_words, &["RESET"]))
            && changes_search_path(statement))
            || (starts_with(&statement_words, &["ALTER", "ROLE"])
                || starts_with(&statement_words, &["ALTER", "USER"])
                || starts_with(&statement_words, &["ALTER", "DATABASE"]))
        {
            push_finding(
                &mut findings,
                "blocked",
                "migration_sql_cannot_change_role_or_search_path_configuration",
                statement_index,
            );
        }
        if controls_transaction_boundary(statement, false) {
            push_finding(
                &mut findings,
                "blocked",
                "migration_sql_cannot_control_transaction_boundary",
                statement_index,
            );
        }

        for token in statement {
            if let TokenKind::Body(body) = &token.kind {
                split_timeout_override_in_body(
                    body,
                    statement_index,
                    allow_role_prefix_grants
                        || (reviewed_partition_installer
                            && (starts_with(&statement_words, &["CREATE", "FUNCTION"])
                                || starts_with(
                                    &statement_words,
                                    &["CREATE", "OR", "REPLACE", "FUNCTION"],
                                )
                                || starts_with(&statement_words, &["DO"]))),
                    &mut findings,
                )?;
                if starts_with(&statement_words, &["DO"])
                    && changes_authorization_identity(&tokenize(body)?)
                {
                    push_finding(
                        &mut findings,
                        "blocked",
                        "migration_sql_cannot_change_authorization_identity",
                        statement_index,
                    );
                }
                if starts_with(&statement_words, &["DO"]) && changes_search_path(&tokenize(body)?) {
                    push_finding(
                        &mut findings,
                        "blocked",
                        "migration_sql_cannot_change_role_or_search_path_configuration",
                        statement_index,
                    );
                }
                if starts_with(&statement_words, &["DO"])
                    && controls_transaction_boundary(&tokenize(body)?, true)
                {
                    push_finding(
                        &mut findings,
                        "blocked",
                        "migration_sql_cannot_control_transaction_boundary",
                        statement_index,
                    );
                }
            }
        }

        if starts_with(&statement_words, &["CREATE"]) {
            has_expansion = true;
            let unique = statement_words.get(1).is_some_and(|word| word == "UNIQUE");
            let index_word = if unique { 2 } else { 1 };
            if statement_words
                .get(index_word)
                .is_some_and(|word| word == "INDEX")
            {
                has_expansion = true;
                let on_word = statement_words
                    .iter()
                    .enumerate()
                    .skip(index_word + 1)
                    .find(|(_, word)| word.as_str() == "ON")
                    .map(|(index, _)| index)
                    .unwrap_or(statement_words.len());
                let concurrent = statement_words[index_word + 1..on_word]
                    .iter()
                    .any(|word| word == "CONCURRENTLY");
                if concurrent {
                    if change.transaction_mode != TransactionMode::NonTransactional {
                        push_finding(
                            &mut findings,
                            "blocked",
                            "concurrent_index_requires_nontransactional_recovery_contract",
                            statement_index,
                        );
                    } else if change.resume_handler.is_none() || change.repair_handler.is_none() {
                        push_finding(
                            &mut findings,
                            "blocked",
                            "concurrent_index_requires_resume_and_repair_handlers",
                            statement_index,
                        );
                    }
                } else if !plain_index_targets_new_empty_table(&statements, offset, change) {
                    push_finding(
                        &mut findings,
                        "blocked",
                        "create_index_requires_concurrently",
                        statement_index,
                    );
                }
                if unique && concurrent {
                    if let Some(name) = index_name(&statement_words, index_word) {
                        concurrent_unique_indexes.insert(name);
                    }
                }
            }
        }

        if starts_with(&statement_words, &["DROP", "INDEX"]) {
            has_contraction = true;
            push_finding(
                &mut findings,
                "requires_n1",
                "index_removal_requires_executed_n1_rehearsal",
                statement_index,
            );
            let concurrent = statement_words
                .iter()
                .skip(2)
                .take_while(|word| word.as_str() != "ON")
                .any(|word| word == "CONCURRENTLY");
            if !concurrent {
                push_finding(
                    &mut findings,
                    "blocked",
                    "drop_index_requires_concurrently",
                    statement_index,
                );
            } else if change.transaction_mode != TransactionMode::NonTransactional
                || change.resume_handler.is_none()
                || change.repair_handler.is_none()
            {
                push_finding(
                    &mut findings,
                    "blocked",
                    "concurrent_index_requires_nontransactional_recovery_contract",
                    statement_index,
                );
            }
        }

        if starts_with(&statement_words, &["REINDEX"])
            && !statement_words.iter().any(|word| word == "CONCURRENTLY")
        {
            push_finding(
                &mut findings,
                "blocked",
                "reindex_requires_concurrently",
                statement_index,
            );
        } else if starts_with(&statement_words, &["REINDEX"])
            && (change.transaction_mode != TransactionMode::NonTransactional
                || change.resume_handler.is_none()
                || change.repair_handler.is_none())
        {
            push_finding(
                &mut findings,
                "blocked",
                "concurrent_index_requires_nontransactional_recovery_contract",
                statement_index,
            );
        }

        if starts_with(&statement_words, &["VACUUM", "FULL"])
            || starts_with(&statement_words, &["CLUSTER"])
        {
            push_finding(
                &mut findings,
                "blocked",
                "table_rewrite_operation_requires_online_replacement",
                statement_index,
            );
        }
        if starts_with(&statement_words, &["TRUNCATE"]) {
            has_contraction = true;
            push_finding(
                &mut findings,
                "blocked",
                "truncate_is_forbidden_in_successor",
                statement_index,
            );
        }
        if starts_with(&statement_words, &["DROP", "TABLE"]) {
            has_contraction = true;
            push_finding(
                &mut findings,
                "blocked",
                "table_drop_requires_separate_data_retention_review",
                statement_index,
            );
        }
        if starts_with(&statement_words, &["DROP"])
            && !starts_with(&statement_words, &["DROP", "INDEX"])
            && !starts_with(&statement_words, &["DROP", "TABLE"])
        {
            has_contraction = true;
            if statement_words.iter().any(|word| word == "CASCADE") {
                push_finding(
                    &mut findings,
                    "blocked",
                    "drop_cascade_is_forbidden_in_successor",
                    statement_index,
                );
            } else {
                push_finding(
                    &mut findings,
                    "requires_n1",
                    "drop_object_requires_executed_n1_rehearsal",
                    statement_index,
                );
            }
        }

        if starts_with(&statement_words, &["UPDATE"])
            && !statement_words.iter().any(|word| word == "WHERE")
        {
            push_finding(
                &mut findings,
                "blocked",
                "unbounded_update_requires_batched_backfill",
                statement_index,
            );
        }
        if starts_with(&statement_words, &["DELETE", "FROM"])
            && !statement_words.iter().any(|word| word == "WHERE")
        {
            push_finding(
                &mut findings,
                "blocked",
                "unbounded_delete_requires_partition_or_batch_plan",
                statement_index,
            );
        }

        if starts_with(&statement_words, &["ALTER", "TABLE"]) {
            if (has_phrase(&statement_words, &["DROP", "COLUMN"])
                || has_phrase(&statement_words, &["DROP", "CONSTRAINT"])
                || has_phrase(&statement_words, &["RENAME", "COLUMN"])
                || has_phrase(&statement_words, &["RENAME", "TO"]))
                && !super::reviewed_schema::validated_widening(reviewed029, statement_index)
            {
                has_contraction = true;
                push_finding(
                    &mut findings,
                    "requires_n1",
                    "contract_ddl_requires_executed_n1_rehearsal",
                    statement_index,
                );
            }
            let alters_column_type = statement_words
                .windows(2)
                .position(|window| window[0] == "ALTER" && window[1] == "COLUMN")
                .is_some_and(|index| {
                    statement_words[index + 2..]
                        .iter()
                        .any(|word| word == "TYPE")
                });
            if alters_column_type || has_phrase(&statement_words, &["SET", "DATA", "TYPE"]) {
                push_finding(
                    &mut findings,
                    "blocked",
                    "column_type_change_requires_expand_backfill_contract",
                    statement_index,
                );
            }
            if has_phrase(&statement_words, &["ADD", "COLUMN"])
                && has_phrase(&statement_words, &["NOT", "NULL"])
                && !super::reviewed_schema::new_empty_table_alter(reviewed029, statement_index)
            {
                push_finding(
                    &mut findings,
                    "blocked",
                    if statement_words.iter().any(|word| word == "DEFAULT") {
                        "add_not_null_default_requires_expand_backfill_contract"
                    } else {
                        "add_not_null_requires_expand_backfill_contract"
                    },
                    statement_index,
                );
            }
            if has_phrase(&statement_words, &["ADD", "COLUMN"])
                && column_default_may_rewrite(statement)
            {
                push_finding(
                    &mut findings,
                    "blocked",
                    "column_default_may_rewrite_table",
                    statement_index,
                );
            }
            if has_phrase(&statement_words, &["ADD", "COLUMN"])
                || has_phrase(&statement_words, &["ADD", "CONSTRAINT"])
            {
                has_expansion = true;
            }
            if has_phrase(&statement_words, &["SET", "NOT", "NULL"]) {
                if staged_not_null {
                    has_contraction = true;
                }
                let has_validated_check = pending_validation.iter().any(|(name, add_statement)| {
                    *add_statement < statement_index
                        && validated_constraints.get(name).is_some_and(|validated_at| {
                            *validated_at > *add_statement && *validated_at < statement_index
                        })
                });
                if !has_validated_check && !staged_not_null {
                    push_finding(
                        &mut findings,
                        "blocked",
                        "set_not_null_requires_prevalidated_check",
                        statement_index,
                    );
                }
            }

            let add_constraint = statement_words
                .windows(2)
                .position(|window| window[0] == "ADD" && window[1] == "CONSTRAINT");
            let unnamed_constraint = statement_words.iter().enumerate().any(|(index, word)| {
                word == "ADD"
                    && statement_words
                        .get(index + 1..)
                        .is_some_and(starts_constraint_kind)
            }) || add_constraint.is_some_and(|index| {
                statement_words
                    .get(index + 2..)
                    .is_some_and(starts_constraint_kind)
            });
            if unnamed_constraint {
                push_finding(
                    &mut findings,
                    "blocked",
                    "unnamed_constraint_requires_explicit_reviewable_name",
                    statement_index,
                );
            }
            if let Some(add_index) = add_constraint {
                let constraint_name = statement_words.get(add_index + 2).cloned();
                let kind_words = &statement_words[add_index + 3..];
                let check_or_foreign = kind_words.iter().any(|word| word == "CHECK")
                    || has_phrase(kind_words, &["FOREIGN", "KEY"]);
                let unique_or_primary = kind_words.iter().any(|word| word == "UNIQUE")
                    || has_phrase(kind_words, &["PRIMARY", "KEY"])
                    || kind_words.iter().any(|word| word == "EXCLUDE");
                if check_or_foreign || unique_or_primary {
                    has_expansion = true;
                }
                if check_or_foreign {
                    if !has_phrase(kind_words, &["NOT", "VALID"]) {
                        push_finding(
                            &mut findings,
                            "blocked",
                            "check_or_foreign_key_requires_not_valid_expand_step",
                            statement_index,
                        );
                    } else if let Some(name) = constraint_name {
                        pending_validation.push((name, statement_index));
                    }
                }
                if unique_or_primary {
                    let using_index = kind_words
                        .windows(2)
                        .position(|window| window[0] == "USING" && window[1] == "INDEX")
                        .and_then(|index| kind_words.get(index + 2));
                    if using_index.is_none_or(|name| !concurrent_unique_indexes.contains(name))
                        && !super::reviewed_schema::new_empty_table_alter(
                            reviewed029,
                            statement_index,
                        )
                    {
                        push_finding(
                            &mut findings,
                            "blocked",
                            "unique_constraint_requires_concurrent_unique_index",
                            statement_index,
                        );
                    }
                }
            }

            if starts_with(&statement_words, &["ALTER", "TABLE"])
                && has_phrase(&statement_words, &["VALIDATE", "CONSTRAINT"])
            {
                if let Some(index) = statement_words
                    .windows(2)
                    .position(|window| window[0] == "VALIDATE" && window[1] == "CONSTRAINT")
                {
                    if let Some(name) = statement_words.get(index + 2) {
                        validated_constraints.insert(name.clone(), statement_index);
                    }
                }
            }
        }
    }

    for (name, statement) in pending_validation {
        if !validated_constraints
            .get(&name)
            .is_some_and(|validated_at| *validated_at > statement)
        {
            push_finding(
                &mut findings,
                "requires_n1",
                "not_valid_constraint_requires_followup_validation_and_n1",
                statement,
            );
        }
    }
    if has_expansion && has_contraction {
        push_finding(
            &mut findings,
            "blocked",
            "expand_and_contract_must_be_separate_successors",
            0,
        );
    }
    if !findings
        .iter()
        .any(|finding| finding.code == "contract_ddl_requires_executed_n1_rehearsal")
    {
        findings.push(Finding {
            severity: "requires_n1".into(),
            code: "successor_requires_executed_n1_rehearsal".into(),
            statement: 0,
        });
    }
    let phase = if has_contraction {
        "contract"
    } else if has_expansion {
        "expand"
    } else {
        "expand"
    };
    Ok((findings, phase))
}
