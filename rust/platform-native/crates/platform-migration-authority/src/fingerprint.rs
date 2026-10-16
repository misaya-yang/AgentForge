//! Exact four-class canonical catalog contract from database/authority/fingerprint.py.
//! SQL snapshots retain source predicates, deparser calls and ordering verbatim.
mod values;
pub use values::{canonical_digest, canonical_value, python_json};
use crate::{manifest::ReferenceDataSet,AuthorityError,Result};
use serde_json::{Value,json};
use sqlx::{Connection,PgConnection};
use std::collections::BTreeMap;
const SCHEMAS:[&str;4]=["gateway","assistant","knowledge","public"];
const PRINCIPALS:[&str;7]=["owner","migrator","gateway","runtime","capability_worker","knowledge_api","knowledge_worker"];
fn refused(s:impl Into<String>)->AuthorityError{AuthorityError::Refused(s.into())}
fn s(row:&Value,key:&str)->String{match &row[key]{Value::String(s)=>s.clone(),Value::Null=>"None".into(),Value::Bool(b)=>if *b{"True"}else{"False"}.into(),v=>v.to_string()}}
fn b(row:&Value,key:&str)->u8{u8::from(row[key].as_bool()==Some(true))}
fn owner(name:&str,prefix:&str)->String{for p in PRINCIPALS{if name==format!("{prefix}{p}"){return p.into();}}name.into()}
pub fn like_prefix_pattern(prefix:&str)->String{format!("{}%",prefix.replace('\\',"\\\\").replace('%',"\\%").replace('_',"\\_"))}
fn query(key:&str)->Result<String>{let all:Value=serde_json::from_str(include_str!("fingerprint/catalog_queries.json")).map_err(|_|refused("invalid bundled catalog queries"))?;all[key].as_str().map(str::to_owned).ok_or_else(||refused("missing catalog query"))}
async fn rows(conn:&mut PgConnection,key:&str,prefix:&str,identity:Option<(&str,&str)>)->Result<Vec<Value>>{
 let sql=format!("SELECT to_jsonb(fingerprint_row) FROM ({}) AS fingerprint_row",query(key)?);
 let mut q=sqlx::query_scalar::<_,Value>(&sql);
 if let Some((schema,name))=identity{q=q.bind(schema).bind(name);}else if matches!(key,"acl_lines/roles"|"database_acl_lines/rows"){q=q.bind(like_prefix_pattern(prefix));}else if key!="extension_catalog_lines/rows"{q=q.bind(SCHEMAS.to_vec());if key=="acl_lines/default_privileges"{q=q.bind(like_prefix_pattern(prefix));}}
 let mut result=q.fetch_all(conn).await?;
 // asyncpg decodes PostgreSQL's internal "char" type as one-byte bytes,
 // including NUL for empty catalog flags. Frozen hashes contain Python bytes
 // repr, not human-friendly enum names. Preserve this existing wire contract.
 let byte_fields:&[&str]=match key{
  "structural_lines/relations"=>&["kind","persistence","replica_identity"],
  "structural_lines/columns"=>&["identity","generated","storage","compression"],
  "structural_lines/constraints"=>&["type"],
  "structural_lines/types"|"acl_lines/type_owners"|"acl_lines/type_acls"=>&["type"],
  "structural_catalog_detail_lines/views"=>&["kind"],
  "structural_catalog_detail_lines/triggers"=>&["enabled"],
  "structural_catalog_detail_lines/aggregates"=>&["aggkind","aggfinalmodify","aggmfinalmodify"],
  "policy_acl_lines/rows"=>&["command"],_=>&[]};
 for row in &mut result{for field in byte_fields{if let Some(value)=row[*field].as_str(){row[*field]=json!(catalog_byte_repr(value));}}}
 Ok(result)
}
fn catalog_byte_repr(value:&str)->String{
 let byte=value.as_bytes().first().copied().unwrap_or(0);
 match byte{0=>"b'\\x00'".into(),b'\''=>"b\"'\"".into(),b'\\'=>"b'\\\\'".into(),32..=126=>format!("b'{}'",byte as char),_=>format!("b'\\x{byte:02x}'")}
}
fn render(row:&Value,template:&str)->Result<String>{let mut out=String::new();let mut rest=template;while let Some(start)=rest.find('{'){out.push_str(&rest[..start]);let end=rest[start..].find('}').ok_or_else(||refused("invalid catalog template"))?+start;out.push_str(&s(row,&rest[start+1..end]));rest=&rest[end+1..];}out.push_str(rest);Ok(out)}
async fn append(conn:&mut PgConnection,lines:&mut Vec<String>,key:&str,template:&str,prefix:&str)->Result<()>{for row in rows(conn,key,prefix,None).await?{lines.push(render(&row,template)?);}Ok(())}
pub async fn structural_lines(conn:&mut PgConnection)->Result<Vec<String>>{
 let mut lines=vec![];
 append(conn,&mut lines,"structural_lines/schemas","schema:{nspname}","").await?;
 for mut r in rows(conn,"structural_lines/relations","",None).await?{
  r["kind"]=json!(match s(&r,"kind").as_str(){"r"=>"table","p"=>"partitioned_table","v"=>"view","m"=>"materialized_view","S"=>"sequence","f"=>"foreign_table",_=>r["kind"].as_str().unwrap_or("")});
  for field in ["is_partition","row_security","force_row_security"]{r[field]=json!(b(&r,field));}
  lines.push(render(&r,"relation:{schema}.{name}:{kind}:persistence={persistence}:partition={is_partition}:rls={row_security}/{force_row_security}:replica_identity={replica_identity}:am={access_method}:partition_key={partition_key}:bound={partition_bound}:options={options}")?);
 }
 for mut r in rows(conn,"structural_lines/columns","",None).await?{r["not_null"]=json!(b(&r,"not_null"));lines.push(render(&r,"column:{schema}.{relation}.{column}:{position}:{type}:notnull={not_null}:default={default_expr}:identity={identity}:generated={generated}:storage={storage}:compression={compression}:collation={collation}")?);}
 for mut r in rows(conn,"structural_lines/constraints","",None).await?{r["validated"]=json!(b(&r,"validated"));lines.push(render(&r,"constraint:{schema}.{relation}.{name}:{type}:validated={validated}:{definition}")?);}
 for mut r in rows(conn,"structural_lines/indexes","",None).await?{let mut flags=String::new();for(f,c)in [("is_unique",'u'),("is_primary",'p'),("is_valid",'v'),("is_ready",'r'),("is_exclusion",'x'),("is_replica_identity",'i'),("is_clustered",'c')]{if b(&r,f)==1{flags.push(c);}}r["flags"]=json!(flags);lines.push(render(&r,"index:{schema}.{name}:{flags}:{definition}")?);}
 for mut r in rows(conn,"structural_lines/sequences","",None).await?{r["cycle"]=json!(b(&r,"cycle"));lines.push(render(&r,"sequence:{schema}.{name}:{data_type}:start={start_value}:by={increment_by}:min={min_value}:max={max_value}:cache={cache_size}:cycle={cycle}")?);}
 for mut r in rows(conn,"structural_lines/functions","",None).await?{r["security"]=json!(if b(&r,"security_definer")==1{"definer"}else{"invoker"});lines.push(render(&r,"function:{schema}.{name}({identity_args}):{security}:{definition}")?);}
 append(conn,&mut lines,"structural_catalog_detail_lines/views","view:{schema}.{name}:{kind}:{definition}","").await?;
 append(conn,&mut lines,"structural_catalog_detail_lines/triggers","trigger:{schema}.{relation}.{name}:enabled={enabled}:{definition}","").await?;
 for mut r in rows(conn,"structural_catalog_detail_lines/aggregates","",None).await?{
  r["functions"]=json!(["transition_function","final_function","combine_function","serial_function","deserial_function","moving_transition_function","moving_inverse_function","moving_final_function"].iter().map(|f|s(&r,f)).collect::<Vec<_>>().join(","));
  for f in ["aggfinalextra","aggmfinalextra"]{r[f]=json!(b(&r,f));}
  lines.push(render(&r,"aggregate:{schema}.{name}({identity_args}):kind={aggkind}:direct={aggnumdirectargs}:functions={functions}:state={transition_type}/{moving_transition_type}:space={aggtransspace}/{aggmtransspace}:extra={aggfinalextra}/{aggmfinalextra}:modify={aggfinalmodify}/{aggmfinalmodify}:sort={sort_operator}:init={initial_value}/{moving_initial_value}")?);
 }
 for mut r in rows(conn,"structural_lines/types","",None).await?{
  r["not_null"]=json!(b(&r,"not_null"));lines.push(render(&r,"type:{schema}.{name}:{type}:base={base_type}:notnull={not_null}:default={default_expr}:collation={collation}:range_subtype={range_subtype}:canonical={canonical}:subtype_diff={subtype_diff}")?);
  let(schema,name)=(s(&r,"schema"),s(&r,"name"));
  let kind=s(&r,"type");let nested=match kind.as_str(){"e"=>Some(("structural_lines/labels","enum:{schema}.{name}:{enumlabel}")),"d"=>Some(("structural_lines/domain_constraints","domain_constraint:{schema}.{name}.{conname}:{definition}")),"c"=>Some(("structural_lines/attributes","composite_attribute:{schema}.{name}.{attribute_name}:{position}:{type}:collation={collation}")),_=>None};
  if let Some((key,template))=nested{for mut item in rows(conn,key,"",Some((&schema,&name))).await?{if kind=="c"{item["attribute_name"]=item["name"].clone();}item["schema"]=json!(schema);item["name"]=json!(name);lines.push(render(&item,template)?);}}
 }Ok(lines)
}
pub async fn acl_lines(conn:&mut PgConnection,prefix:&str)->Result<Vec<String>>{
 let mut lines=vec![];
 for(key,template)in [
  ("acl_lines/owners","owner:{schema}.{name}:{owner}"),
  ("acl_lines/schema_owners","schema_owner:{schema}:{owner}"),
  ("acl_lines/function_owners","function_owner:{schema}.{name}({identity_args}):{owner}"),
  ("acl_lines/type_owners","type_owner:{schema}.{name}:{type}:{owner}"),
  ("acl_lines/acls","acl:{schema}.{name}:{grantee}:{privileges}:grantopt={grant_option}"),
  ("column_acl_lines/rows","column_acl:{schema}.{relation}.{column}:{grantee}:{privileges}:grantopt={grant_option}"),
  ("acl_lines/function_acls","function_acl:{schema}.{name}({identity_args}):{grantee}:{privileges}:grantopt={grant_option}"),
  ("acl_lines/schema_acls","schema_acl:{schema}:{grantee}:{privileges}:grantopt={grant_option}"),
  ("database_acl_lines/rows","database_acl:{grantee}:{privileges}:grantopt={grant_option}"),
  ("acl_lines/type_acls","type_acl:{schema}.{name}:{type}:{grantee}:{privileges}:grantopt={grant_option}"),
  ("policy_acl_lines/rows","policy:{schema}.{relation}.{name}:permissive={permissive}:command={command}:roles={roles}:using={using_expr}:check={check_expr}"),
  ("acl_lines/default_privileges","default_privilege:{grantor}:{object_type}:{schema}:{grantee}:{privileges}:grantopt={grant_option}"),
  ("acl_lines/roles","role:{name}:{flags}:{config}:memberships={memberships}")]{
  for mut r in rows(conn,key,prefix,None).await?{
   for f in ["owner","grantee","grantor"]{if r.get(f).is_some(){r[f]=json!(owner(&s(&r,f),prefix));}}
   for f in ["grant_option","permissive"]{if r.get(f).is_some(){r[f]=json!(b(&r,f));}}
   if key=="policy_acl_lines/rows"{r["roles"]=json!(r["roles"].as_array().map(|a|a.iter().map(|v|owner(v.as_str().unwrap_or(""),prefix)).collect::<Vec<_>>()).unwrap_or_default().join(","));}
   if key=="acl_lines/roles"{
    r["name"]=json!(owner(&s(&r,"name"),prefix));let mut flags=String::new();for(f,c)in [("rolsuper",'S'),("rolcreaterole",'R'),("rolcreatedb",'D'),("rolcanlogin",'L'),("rolinherit",'I'),("rolreplication",'P'),("rolbypassrls",'B')]{if b(&r,f)==1{flags.push(c);}}r["flags"]=json!(flags);
    let mut memberships=vec![];if let Some(items)=r["memberships"].as_array(){for item in items{let text=item.as_str().ok_or_else(||refused("invalid role membership"))?;let parts=text.rsplitn(4,':').collect::<Vec<_>>();if parts.len()!=4{return Err(refused("invalid role membership"));}memberships.push(format!("{}:admin={}:inherit={}:set={}",owner(parts[3],prefix),parts[2],parts[1],parts[0]));}}r["memberships"]=json!(memberships.join(","));
   }lines.push(render(&r,template)?);
  }
 }Ok(lines)
}
pub async fn extensions_lines(conn:&mut PgConnection,prefix:&str)->Result<Vec<String>>{let mut lines=vec![];for mut r in rows(conn,"extension_catalog_lines/rows",prefix,None).await?{let name=s(&r,"extname");if name=="plpgsql"{continue;}if !["uuid-ossp","pgcrypto","pg_trgm","vector"].contains(&name.as_str()){return Err(refused(format!("extension {name:?} is not in the allowlist")));}r["owner"]=json!(owner(&s(&r,"owner"),prefix));lines.push(render(&r,"extension:{extname}:{extversion}:schema={schema}:owner={owner}")?);}Ok(lines)}
pub async fn reference_data_lines(conn:&mut PgConnection,sets:&[ReferenceDataSet])->Result<Vec<String>>{
 let mut sorted=sets.iter().collect::<Vec<_>>();sorted.sort_by(|a,b|a.table.cmp(&b.table));let mut lines=vec![];
 for set in sorted{
  let mut columns=vec![];for c in set.natural_key.iter().chain(&set.immutable_columns){crate::manifest::validate_identifier(c)?;if !columns.contains(c){columns.push(c.clone());}}
  if set.table.split('.').count()>2 || set.table.split('.').any(|part|crate::manifest::validate_identifier(part).is_err()) || [";","--","/*","*/"].iter().any(|x|set.where_clause.contains(x)) || set.where_clause.chars().any(|c|(c as u32)<32){return Err(refused("invalid reference data selection"));}
  let expressions=columns.iter().map(|c|format!("jsonb_build_object('type',pg_typeof({c})::text,'kind',(SELECT typtype::text FROM pg_type WHERE oid=pg_typeof({c})::oid),'base',(SELECT format_type(typbasetype,NULL) FROM pg_type WHERE oid=pg_typeof({c})::oid),'value',to_jsonb({c}),'text',{c}::text)")).collect::<Vec<_>>();
  let predicate=if set.where_clause.is_empty(){String::new()}else{format!(" WHERE {}",set.where_clause)};
  let sql=format!("SELECT jsonb_build_array({}) FROM {}{}",expressions.join(","),set.table,predicate);
  let rows=sqlx::query_scalar::<_,Value>(&sql).fetch_all(&mut *conn).await?;let count=rows.len();let mut canonical=vec![];
  for row in rows{let mut tagged=vec![];for item in row.as_array().ok_or_else(||refused("invalid reference row"))?{tagged.push(canonical_value(item)?);}canonical.push(python_json(&Value::Array(tagged)));}canonical.sort();
  lines.push(format!("reference:{}:columns={}:rows={count}",set.table,columns.join(",")));lines.extend(canonical.into_iter().map(|r|format!("row:{}:{r}",set.table)));
 }Ok(lines)
}
pub async fn compute_fingerprints(conn:&mut PgConnection,prefix:&str,reference_sets:&[ReferenceDataSet])->Result<BTreeMap<String,String>>{
 let mut tx=conn.begin().await?;let previous:String=sqlx::query_scalar("SELECT current_setting('search_path')").fetch_one(&mut *tx).await?;
 sqlx::query("SELECT set_config('search_path','pg_catalog',true)").execute(&mut *tx).await?;
 let mut fingerprints=BTreeMap::new();fingerprints.insert("structural".into(),canonical_digest(&structural_lines(&mut tx).await?));fingerprints.insert("acl".into(),canonical_digest(&acl_lines(&mut tx,prefix).await?));fingerprints.insert("extensions".into(),canonical_digest(&extensions_lines(&mut tx,prefix).await?));fingerprints.insert("reference_data".into(),canonical_digest(&reference_data_lines(&mut tx,reference_sets).await?));
 sqlx::query("SELECT set_config('search_path',$1,true)").bind(previous).execute(&mut *tx).await?;tx.commit().await?;Ok(fingerprints)
}
#[cfg(test)]mod tests{use super::*;#[test]fn literal_role_prefix(){assert_eq!(like_prefix_pattern("a_b%"),"a\\_b\\%%");assert_eq!(owner("ai_gateway_capability_worker","ai_gateway_"),"capability_worker");assert_eq!(owner("postgres","ai_gateway_"),"postgres");}#[test]fn catalog_inventory(){let v:Value=serde_json::from_str(include_str!("fingerprint/catalog_queries.json")).unwrap();assert_eq!(v.as_object().unwrap().len(),28);assert!(query("structural_lines/constraints").unwrap().contains("FROM pg_constraint AS con"));assert!(query("structural_lines/domain_constraints").unwrap().contains("contypeid")==false);}}
#[cfg(test)]mod asyncpg_char_parity{use super::*;#[test]fn internal_char_frozen_repr(){assert_eq!(catalog_byte_repr("r"),"b'r'");assert_eq!(catalog_byte_repr(""),"b'\\x00'");assert_eq!(catalog_byte_repr("O"),"b'O'");assert_eq!(catalog_byte_repr("'"),"b\"'\"");}}
