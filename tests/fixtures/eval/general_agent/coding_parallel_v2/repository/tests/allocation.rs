use settlement_incident_fixture::allocation::allocate_cents;
#[test]
fn allocation_preserves_every_cent_and_breaks_ties_by_id() {
    let allocations = allocate_cents(10, &[("merchant-c",1),("merchant-a",1),("merchant-b",1)]).unwrap();
    assert_eq!(allocations, [("merchant-a".into(),4),("merchant-b".into(),3),("merchant-c".into(),3)].into());
    assert_eq!(allocations.values().sum::<i64>(),10);
}
#[test]
fn allocation_uses_largest_fractional_remainder() {
    let allocations = allocate_cents(11, &[("reserve",1),("seller",3),("tax",2)]).unwrap();
    assert_eq!(allocations, [("reserve".into(),2),("seller".into(),5),("tax".into(),4)].into());
    assert_eq!(allocations.values().sum::<i64>(),11);
}
