local p=redis.call('XPENDING',KEYS[1],ARGV[1],ARGV[2],ARGV[2],1)
if #p==0 then return {0,false} end
if p[1][2]~=ARGV[3] or tostring(p[1][4])~=ARGV[4] then return {-1,false} end
if ARGV[5]=='0' then return {1,{}} end
local entry=redis.call('XRANGE',KEYS[1],ARGV[2],ARGV[2],'COUNT',1)
if #entry==0 then return {-1,false} end
return {1,entry[1][2]}
