trim_safe(KEYS[1])
if redis.call('XLEN',KEYS[1])>=tonumber(ARGV[1]) then return {0,'full'} end
return {1,redis.call('XADD',KEYS[1],'*',unpack(ARGV,2))}
