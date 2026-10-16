use super::*;
const COMMON: &str = include_str!("streams.lua");
pub(super) struct RedisBackend {
    client: redis::Client,
    connection: Mutex<Option<redis::aio::MultiplexedConnection>>,
    cursors: Mutex<HashMap<(String, String), String>>,
}
impl RedisBackend {
    pub fn new(client: redis::Client) -> Self {
        Self {
            client,
            connection: Mutex::new(None),
            cursors: Mutex::new(HashMap::new()),
        }
    }
    async fn connection(&self) -> Result<redis::aio::MultiplexedConnection, QueueError> {
        let mut slot = self.connection.lock().await;
        if slot.is_none() {
            *slot = Some(
                tokio::time::timeout(
                    Duration::from_secs(5),
                    self.client.get_multiplexed_async_connection_with_config(
                        &redis::AsyncConnectionConfig::new()
                            .set_connection_timeout(Some(Duration::from_secs(5)))
                            .set_response_timeout(Some(Duration::from_secs(5)))
                            .set_concurrency_limit(128)
                            .set_pipeline_buffer_size(32)
                            .set_write_backpressure_boundary(65536),
                    ),
                )
                .await
                .map_err(|_| QueueError::Backend)?
                .map_err(|_| QueueError::Backend)?,
            );
        }
        Ok(slot.as_ref().unwrap().clone())
    }
    async fn query<T: redis::FromRedisValue>(&self, cmd: &redis::Cmd) -> Result<T, QueueError> {
        let mut c = self.connection().await?;
        match tokio::time::timeout(Duration::from_secs(5), cmd.query_async(&mut c)).await {
            Ok(Ok(value)) => Ok(value),
            _ => {
                self.connection.lock().await.take();
                Err(QueueError::Backend)
            }
        }
    }
}
impl QueueBackend for RedisBackend {
    fn ensure_group<'a>(&'a self, stream: &'a str, group: &'a str) -> QueueFuture<'a, ()> {
        Box::pin(async move {
            // Handle BUSYGROUP inside Lua so genuine network errors remain errors.
            let script = "local r=redis.pcall('XGROUP','CREATE',KEYS[1],ARGV[1],'0','MKSTREAM'); if type(r)=='table' and r.err and not string.find(r.err,'BUSYGROUP',1,true) then return redis.error_reply(r.err) end; return 1";
            let _: i64 = self
                .query(redis::cmd("EVAL").arg(script).arg(1).arg(stream).arg(group))
                .await?;
            Ok(())
        })
    }
    fn add<'a>(
        &'a self,
        stream: &'a str,
        fields: &'a [(String, String)],
        max_len: usize,
    ) -> QueueFuture<'a, String> {
        Box::pin(async move {
            let script = format!("{COMMON}\n{}", include_str!("publish.lua"));
            let mut cmd = redis::cmd("EVAL");
            cmd.arg(script).arg(1).arg(stream).arg(max_len);
            for (k, v) in fields {
                cmd.arg(k).arg(v);
            }
            let (status, id): (i64, String) = self.query(&cmd).await?;
            if status == 0 {
                Err(QueueError::Backpressure)
            } else {
                Ok(id)
            }
        })
    }
    fn read<'a>(
        &'a self,
        stream: &'a str,
        group: &'a str,
        consumer: &'a str,
        idle_ms: u64,
        excluded: &'a [String],
    ) -> QueueFuture<'a, Option<QueueMessage>> {
        Box::pin(async move {
            let key = (stream.into(), group.into());
            let cursor = self
                .cursors
                .lock()
                .await
                .get(&key)
                .cloned()
                .unwrap_or("-".into());
            let mut cmd = redis::cmd("EVAL");
            cmd.arg(include_str!("read.lua"))
                .arg(1)
                .arg(stream)
                .arg(group)
                .arg(consumer)
                .arg(idle_ms)
                .arg(cursor)
                .arg(excluded);
            let (cursor, message): (String, Option<(String, Vec<String>, i64)>) =
                self.query(&cmd).await?;
            self.cursors.lock().await.insert(key, cursor);
            message
                .map(|(id, flat, deliveries)| {
                    if flat.len() % 2 != 0 {
                        return Err(QueueError::InvalidValue);
                    }
                    Ok(QueueMessage {
                        id,
                        deliveries,
                        fields: flat
                            .chunks_exact(2)
                            .map(|p| (p[0].clone(), p[1].clone()))
                            .collect(),
                    })
                })
                .transpose()
        })
    }
    fn settle<'a>(
        &'a self,
        stream: &'a str,
        group: &'a str,
        id: &'a str,
        consumer: &'a str,
        deliveries: i64,
        target: Option<(&'a str, &'a [(String, String)])>,
        max_len: usize,
    ) -> QueueFuture<'a, i64> {
        Box::pin(async move {
            if target.is_some_and(|(t, _)| t == stream) {
                return Err(QueueError::InvalidValue);
            }
            let script = format!("{COMMON}\n{}", include_str!("settle.lua"));
            let mut cmd = redis::cmd("EVAL");
            cmd.arg(script)
                .arg(if target.is_some() { 2 } else { 1 })
                .arg(stream);
            if let Some((target, _)) = target {
                cmd.arg(target);
            }
            cmd.arg(group)
                .arg(id)
                .arg(consumer)
                .arg(deliveries)
                .arg(max_len);
            if let Some((_, fields)) = target {
                for (k, v) in fields {
                    cmd.arg(k).arg(v);
                }
            }
            match self.query::<i64>(&cmd).await? {
                -1 => Err(QueueError::StaleDelivery),
                -2 => Err(QueueError::Backpressure),
                n => Ok(n),
            }
        })
    }
    fn inspect<'a>(
        &'a self,
        stream: &'a str,
        group: &'a str,
        id: &'a str,
        consumer: &'a str,
        deliveries: i64,
        fields: bool,
    ) -> QueueFuture<'a, Option<HashMap<String, String>>> {
        Box::pin(async move {
            let (status, flat): (i64, Option<Vec<String>>) = self
                .query(
                    redis::cmd("EVAL")
                        .arg(include_str!("inspect.lua"))
                        .arg(1)
                        .arg(stream)
                        .arg(group)
                        .arg(id)
                        .arg(consumer)
                        .arg(deliveries)
                        .arg(if fields { 1 } else { 0 }),
                )
                .await?;
            match status {
                0 => Ok(None),
                -1 => Err(QueueError::StaleDelivery),
                _ => {
                    let flat = flat.ok_or(QueueError::InvalidValue)?;
                    if flat.len() % 2 != 0 {
                        return Err(QueueError::InvalidValue);
                    }
                    Ok(Some(
                        flat.chunks_exact(2)
                            .map(|p| (p[0].clone(), p[1].clone()))
                            .collect(),
                    ))
                }
            }
        })
    }
    fn close(&self) -> QueueFuture<'_, ()> {
        Box::pin(async move {
            self.connection.lock().await.take();
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
        net::TcpListener,
    };
    async fn command(
        reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    ) -> Option<Vec<String>> {
        let mut line = String::new();
        if reader.read_line(&mut line).await.unwrap() == 0 {
            return None;
        }
        let count: usize = line.trim().strip_prefix('*').unwrap().parse().unwrap();
        let mut result = vec![];
        for _ in 0..count {
            line.clear();
            reader.read_line(&mut line).await.unwrap();
            let n: usize = line.trim().strip_prefix('$').unwrap().parse().unwrap();
            let mut data = vec![0; n + 2];
            reader.read_exact(&mut data).await.unwrap();
            result.push(String::from_utf8(data[..n].to_vec()).unwrap());
        }
        Some(result)
    }
    #[tokio::test]
    async fn redis_wire_uses_fenced_atomic_eval_and_decodes_reclaimed_delivery() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (r, mut w) = stream.into_split();
            let mut r = BufReader::new(r);
            let mut stage = 0;
            while let Some(c) = command(&mut r).await {
                if c[0] == "CLIENT" {
                    w.write_all(b"+OK\r\n").await.unwrap();
                    continue;
                }
                assert_eq!(c[0], "EVAL");
                let response: &[u8] = match stage {
                    0 => {
                        assert!(c[1].contains("XGROUP"));
                        b":1\r\n"
                    }
                    1 => {
                        assert!(c[1].contains("XPENDING"));
                        assert!(c[1].contains("XCLAIM"));
                        assert!(!c[1].contains("'IDLE'"));
                        assert!(c[1].contains("p[3]>=tonumber(ARGV[3])"));
                        assert_eq!(
                            &c[2..],
                            &["1", "{q}:main", "group", "worker", "60000", "-", "1-0"]
                        );
                        b"*2\r\n$4\r\n(2-0\r\n*3\r\n$3\r\n2-0\r\n*2\r\n$7\r\npayload\r\n$2\r\n{}\r\n:2\r\n"
                    }
                    2 => {
                        assert!(c[1].contains("p[1][2]~=ARGV[3]"));
                        assert!(c[1].find("'XADD'").unwrap() < c[1].find("'XACK'").unwrap());
                        assert_eq!(
                            &c[2..],
                            &[
                                "2",
                                "{q}:main",
                                "queue:retry:{q}:main",
                                "group",
                                "2-0",
                                "worker",
                                "2",
                                "100",
                                "payload",
                                "{}"
                            ]
                        );
                        b":1\r\n"
                    }
                    _ => panic!("unexpected command"),
                };
                w.write_all(response).await.unwrap();
                stage += 1;
                if stage == 3 {
                    break;
                }
            }
            assert_eq!(stage, 3);
        });
        let b = RedisBackend::new(redis::Client::open(format!("redis://{addr}")).unwrap());
        b.ensure_group("{q}:main", "group").await.unwrap();
        let m = b
            .read("{q}:main", "group", "worker", 60000, &["1-0".into()])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(m.deliveries, 2);
        assert_eq!(m.fields["payload"], "{}");
        assert_eq!(
            b.settle(
                "{q}:main",
                "group",
                "2-0",
                "worker",
                2,
                Some(("queue:retry:{q}:main", &[("payload".into(), "{}".into())])),
                100
            )
            .await
            .unwrap(),
            1
        );
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
    }
}
