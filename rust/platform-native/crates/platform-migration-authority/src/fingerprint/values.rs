//! Type tags and scalar normalization match fingerprint_values.py, not JSON coercion.
use crate::{AuthorityError,Result};
use chrono::{DateTime,NaiveDate,NaiveDateTime,NaiveTime,Utc};
use serde_json::{Value,json};
use sha2::{Digest,Sha256};
fn refused(s:&str)->AuthorityError{AuthorityError::Refused(s.into())}
pub fn canonical_digest(lines:&[String])->String{Sha256::digest(lines.join("\n").as_bytes()).iter().map(|b|format!("{b:02x}")).collect()}
pub fn python_json(value:&Value)->String{
 let raw=serde_json::to_string(value).expect("JSON value");let mut out=String::new();
 for c in raw.chars(){if c<='~'{out.push(c);}else{for unit in c.encode_utf16(&mut [0;2]){out.push_str(&format!("\\u{unit:04x}"));}}}out
}
fn float_hex(value:f64)->String{
 if value.is_nan(){return "nan".into();}if value.is_infinite(){return if value.is_sign_positive(){"+inf"}else{"-inf"}.into();}
 let bits=value.to_bits();let sign=if bits>>63==1{"-"}else{""};let exponent=((bits>>52)&0x7ff)as i32;let fraction=bits&0x000f_ffff_ffff_ffff;
 if exponent==0 && fraction==0{return format!("{sign}0x0.0p+0");}
 if exponent==0{format!("{sign}0x0.{fraction:013x}p-1022")}else{format!("{sign}0x1.{fraction:013x}p{:+}",exponent-1023)}
}
fn decimal(text:&str)->Result<String>{
 if matches!(text,"NaN"|"Infinity"|"-Infinity"){return Ok(text.into());}
 let negative=text.starts_with('-');let raw=text.trim_start_matches(['-','+']);let (mantissa,exp)=if let Some(i)=raw.find(['e','E']){(&raw[..i],raw[i+1..].parse::<i32>().map_err(|_|refused("invalid decimal exponent"))?)}else{(raw,0)};
 let mut digits=String::new();let mut scale=0i32;let mut after=false;
 for c in mantissa.chars(){if c=='.'&&!after{after=true;}else if c.is_ascii_digit(){digits.push(c);if after{scale+=1;}}else{return Err(refused("invalid decimal value"));}}
 scale-=exp;digits=digits.trim_start_matches('0').into();if digits.is_empty(){return Ok("0".into());}
 // Decimal.normalize() uses Python's default 28-digit, half-even context.
 if digits.len()>28{let rest=digits.split_off(28);let first=rest.as_bytes()[0];let round=first>b'5'||(first==b'5'&&(rest.as_bytes()[1..].iter().any(|b|*b!=b'0')||(digits.as_bytes()[27]-b'0')%2==1));scale-=rest.len()as i32;
  if round{let mut bytes=digits.into_bytes();let mut carry=true;for b in bytes.iter_mut().rev(){if *b==b'9'{*b=b'0';}else{*b+=1;carry=false;break;}}if carry{bytes.insert(0,b'1');}digits=String::from_utf8(bytes).map_err(|_|refused("invalid decimal"))?;}
 }
 while digits.ends_with('0'){digits.pop();scale-=1;}
 if scale.abs()>200000{return Err(refused("decimal normalization too large"));}
 let mut out=if scale<=0{format!("{}{}",digits,"0".repeat((-scale)as usize))}else if scale as usize>=digits.len(){format!("0.{}{}","0".repeat(scale as usize-digits.len()),digits)}else{let i=digits.len()-scale as usize;format!("{}.{}",&digits[..i],&digits[i..])};
 if negative{out.insert(0,'-');}Ok(out)
}
fn base64(bytes:&[u8])->String{const A:&[u8]=b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";let mut out=String::new();for b in bytes.chunks(3){let n=((b[0]as u32)<<16)|((b.get(1).copied().unwrap_or(0)as u32)<<8)|(b.get(2).copied().unwrap_or(0)as u32);out.push(A[((n>>18)&63)as usize]as char);out.push(A[((n>>12)&63)as usize]as char);out.push(if b.len()>1{A[((n>>6)&63)as usize]as char}else{'='});out.push(if b.len()>2{A[(n&63)as usize]as char}else{'='});}out}
fn bytes(text:&str)->Result<Vec<u8>>{
 if let Some(raw)=text.strip_prefix("\\x"){
  if raw.len()%2!=0{return Err(refused("invalid bytea"));}
  return (0..raw.len()).step_by(2).map(|i|u8::from_str_radix(&raw[i..i+2],16).map_err(|_|refused("invalid bytea"))).collect();
 }
 // bytea_output=escape is an equally valid session setting.
 let raw=text.as_bytes();let mut result=vec![];let mut i=0;
 while i<raw.len(){if raw[i]!=b'\\'{result.push(raw[i]);i+=1;}else if raw.get(i+1)==Some(&b'\\'){result.push(b'\\');i+=2;}else{
  if i+3>=raw.len(){return Err(refused("invalid bytea escape"));}
  let octal=std::str::from_utf8(&raw[i+1..i+4]).map_err(|_|refused("invalid bytea escape"))?;
  result.push(u8::from_str_radix(octal,8).map_err(|_|refused("invalid bytea escape"))?);i+=4;
 }}Ok(result)
}
fn array_text(text:&str,ty:&str)->Result<Value>{
    // PostgreSQL array text retains exact NUMERIC/float representations and quoting.
    // asyncpg exposes nested lists, ignoring non-default array lower bounds.
    let text=if text.starts_with('['){text.split_once('=').ok_or_else(||refused("invalid array bounds"))?.1}else{text};
    struct Parser<'a>{bytes:&'a [u8],pos:usize,ty:&'a str}
    impl Parser<'_>{
        fn list(&mut self)->Result<Value>{
            if self.bytes.get(self.pos)!=Some(&b'{'){return Err(refused("invalid reference array"));}self.pos+=1;let mut items=vec![];
            if self.bytes.get(self.pos)==Some(&b'}'){self.pos+=1;return Ok(json!(["list",items]));}
            loop{
                if self.bytes.get(self.pos)==Some(&b'{'){items.push(self.list()?);}else{
                    let quoted=self.bytes.get(self.pos)==Some(&b'"');if quoted{self.pos+=1;}
                    let mut raw=vec![];let mut closed=!quoted;
                    while let Some(&c)=self.bytes.get(self.pos){
                        if quoted && c==b'"'{self.pos+=1;closed=true;break;}
                        if !quoted && matches!(c,b','|b'}'){break;}
                        self.pos+=1;if c==b'\\'{let c=*self.bytes.get(self.pos).ok_or_else(||refused("invalid array escape"))?;self.pos+=1;raw.push(c);}else{raw.push(c);}
                    }
                    if !closed{return Err(refused("unterminated array value"));}
                    let text=String::from_utf8(raw).map_err(|_|refused("invalid array UTF8"))?;
                    if !quoted && text=="NULL"{items.push(json!(["null"]));}else{
                        let value=if matches!(self.ty,"boolean"|"bool"){json!(matches!(text.as_str(),"t"|"true"))}else{json!(text)};
                        items.push(scalar(self.ty,&value,&text)?);
                    }
                }
                match self.bytes.get(self.pos){Some(b',')=>self.pos+=1,Some(b'}')=>{self.pos+=1;break;},_=>return Err(refused("invalid array separator"))}
            }Ok(json!(["list",items]))
        }
    }
    let mut p=Parser{bytes:text.as_bytes(),pos:0,ty};let out=p.list()?;if p.pos!=p.bytes.len(){return Err(refused("trailing array text"));}Ok(out)
}
fn scalar(ty:&str,value:&Value,text:&str)->Result<Value>{
 let out=match ty{
  "boolean"|"bool"=>json!(["bool",value.as_bool().ok_or_else(||refused("invalid boolean reference"))?]),
  "smallint"|"integer"|"bigint"|"int2"|"int4"|"int8"|"oid"=>json!(["int",text]),
  "numeric"|"decimal"=>json!(["decimal",decimal(text)?]),
  "real"|"float4"=>json!(["float",float_hex(text.parse::<f32>().map_err(|_|refused("invalid float"))? as f64)]),
  "double precision"|"float8"=>json!(["float",float_hex(text.parse::<f64>().map_err(|_|refused("invalid float"))?)]),
  "text"|"character varying"|"character"|"varchar"|"bpchar"|"name"|"json"|"jsonb"=>json!(["str",text]),
  "bytea"=>json!(["bytes",base64(&bytes(text)?)]),
  "\"char\""=>json!(["bytes",base64(&[text.as_bytes().first().copied().unwrap_or(0)])]),
  "uuid"=>json!(["uuid",uuid::Uuid::parse_str(text).map_err(|_|refused("invalid uuid"))?.to_string()]),
  "date"=>{let text=match text{"infinity"=>"9999-12-31","-infinity"=>"0001-01-01",_=>text};let d=NaiveDate::parse_from_str(text,"%Y-%m-%d").map_err(|_|refused("unsupported reference date"))?;json!(["date",d.format("%Y-%m-%d").to_string()])},
  "timestamp without time zone"|"timestamp"=>{let text=match text{"infinity"=>"9999-12-31 23:59:59.999999","-infinity"=>"0001-01-01 00:00:00",_=>text};let d=NaiveDateTime::parse_from_str(text,"%Y-%m-%d %H:%M:%S%.f").map_err(|_|refused("unsupported reference timestamp"))?;json!(["datetime",d.format("%Y-%m-%dT%H:%M:%S%.6f").to_string()])},
  "timestamp with time zone"|"timestamptz"=>{let parsed=DateTime::parse_from_str(text,"%Y-%m-%d %H:%M:%S%.f%#z").or_else(|_|DateTime::parse_from_rfc3339(text)).map_err(|_|refused("unsupported zoned reference timestamp"))?;json!(["datetime",parsed.with_timezone(&Utc).format("%Y-%m-%dT%H:%M:%S%.6f+00:00").to_string()])},
  "time without time zone"|"time"=>{let time=NaiveTime::parse_from_str(text,"%H:%M:%S%.f").map_err(|_|refused("unsupported reference time"))?;json!(["time",time.format("%H:%M:%S%.6f").to_string()])},
  "time with time zone"|"timetz"=>{let d=DateTime::parse_from_str(&format!("2000-01-01 {text}"),"%Y-%m-%d %H:%M:%S%.f%#z").map_err(|_|refused("unsupported zoned reference time"))?;json!(["time",d.format("%H:%M:%S%.6f%:z").to_string()])},
  _=>return Err(refused(&format!("unsupported reference-data PostgreSQL type: {ty}"))),
 };Ok(out)
}
pub fn canonical_value(envelope:&Value)->Result<Value>{
 let ty=envelope["type"].as_str().ok_or_else(||refused("missing reference type"))?;let value=&envelope["value"];if value.is_null() && envelope["text"].is_null(){return Ok(json!(["null"]));}
 let text=envelope["text"].as_str().ok_or_else(||refused("missing reference text"))?;
 if let Some(base)=ty.strip_suffix("[]"){array_text(text,base)}else if envelope["kind"]=="e"{Ok(json!(["str",text]))}else if envelope["kind"]=="d"{let base=envelope["base"].as_str().ok_or_else(||refused("missing domain base type"))?;if let Some(element)=base.strip_suffix("[]"){array_text(text,element)}else{scalar(base,value,text)}}else{scalar(ty,value,text)}
}
#[cfg(test)]mod tests{
 use super::*;
 #[test]fn python_float_hex(){assert_eq!(float_hex(0.1),"0x1.999999999999ap-4");assert_eq!(float_hex(-0.),"-0x0.0p+0");assert_eq!(float_hex(f64::from_bits(1)),"0x0.0000000000001p-1022");assert_eq!(float_hex(f64::INFINITY),"+inf");}
 #[test]fn decimal_normalization(){for(input,expected)in[("-0.000","0"),("1.2300","1.23"),("1000.0","1000"),("1E-5","0.00001"),("12345678901234567890123456789","12345678901234567890123456790")]{assert_eq!(decimal(input).unwrap(),expected);}}
 #[test]fn array_preserves_types(){let value=canonical_value(&json!({"type":"character varying[]","value":["a",null,"中"],"text":"{a,NULL,中}"})).unwrap();assert_eq!(python_json(&value),r#"["list",[["str","a"],["null"],["str","\u4e2d"]]]"#);}
 #[test]fn binary_and_unicode(){assert_eq!(base64(&[0,1,255]),"AAH/");assert_eq!(python_json(&json!(["😀","\u{7f}"])),r#"["\ud83d\ude00","\u007f"]"#);}
 #[test]fn digest_empty(){assert_eq!(canonical_digest(&[]),"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");}
}
#[cfg(test)]mod parity_goldens{use super::*;#[test]fn original_python_value_fixtures(){let cases:Value=serde_json::from_str(include_str!("value_goldens.json")).unwrap();for case in cases.as_array().unwrap(){assert_eq!(canonical_value(&case["envelope"]).unwrap(),case["expected"],"type={}",case["envelope"]["type"]);}}}
