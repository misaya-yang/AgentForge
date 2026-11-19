//! The authoritative source local embedding is BLAKE2b feature hashing, not a
//! sentence-transformer model. These vectors pin the Python numeric contract.
use ai_platform_knowledge_service::migration_knowledge_query::embedding::local_hash_embedding;
#[test]
fn blake2b_digest_sign_index_and_normalization_match_source() {
    let third = -1.0 / 3.0_f64.sqrt();
    let expected = [0., third, 0., 0., 0., 0., third, third];
    let actual = local_hash_embedding("hello 中文 test42", 8);
    assert!(
        actual
            .iter()
            .zip(expected)
            .all(|(a, b)| (a - b).abs() < 1e-14)
    );
    assert_eq!(
        local_hash_embedding("HELLO hello", 8),
        vec![0., 0., 0., 0., 0., 0., 0., -1.]
    );
}
#[test]
fn token_alphabet_does_not_silently_expand() {
    assert_eq!(local_hash_embedding("مرحبا", 8), vec![0.; 8]);
    assert_eq!(local_hash_embedding("かな 한글", 8), vec![0.; 8]);
    let actual = local_hash_embedding("中文。测试", 8);
    let expected = [
        0.,
        -1. / 2.0_f64.sqrt(),
        0.,
        0.,
        0.,
        0.,
        -1. / 2.0_f64.sqrt(),
        0.,
    ];
    assert!(
        actual
            .iter()
            .zip(expected)
            .all(|(a, b)| (a - b).abs() < 1e-14)
    );
}
#[test]
fn empty_and_repeated_tokens_keep_source_vector_contract() {
    assert_eq!(local_hash_embedding("", 384), vec![0.; 384]);
    assert_eq!(
        local_hash_embedding("hello", 384),
        local_hash_embedding("hello hello", 384)
    );
}
