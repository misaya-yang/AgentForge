-- Fence against a worker that has lost its PEL delivery to another consumer.
local p=redis.call('XPENDING',KEYS[1],ARGV[1],ARGV[2],ARGV[2],1)
if #p==0 then return 0 end
if p[1][2]~=ARGV[3] or tostring(p[1][4])~=ARGV[4] then return -1 end
if #KEYS==2 then
  -- Preflight capacity/type before writing. Lua execution is atomic, not rollback.
  trim_safe(KEYS[2])
  if redis.call('XLEN',KEYS[2])>=tonumber(ARGV[5]) then return -2 end
  redis.call('XADD',KEYS[2],'*',unpack(ARGV,6))
end
redis.call('XACK',KEYS[1],ARGV[1],ARGV[2])
-- Best-effort empty consumer cleanup cannot fail an already completed transfer.
if #redis.call('XPENDING',KEYS[1],ARGV[1],'-','+',1,ARGV[3])==0 then redis.pcall('XGROUP','DELCONSUMER',KEYS[1],ARGV[1],ARGV[3]) end
-- Leave history pruning to bounded admission.
return 1
