-- At most 32 PEL entries scanned per call; cursor advances even over local work.
local start=ARGV[4]
local pending=redis.call('XPENDING',KEYS[1],ARGV[1],start,'+',32)
local cursor='-'
local excluded={}
for i=5,#ARGV do excluded[ARGV[i]]=true end
for _,p in ipairs(pending) do
  cursor='('..p[1]
  if p[3]>=tonumber(ARGV[3]) and not excluded[p[1]] then
    local claimed=redis.call('XCLAIM',KEYS[1],ARGV[1],ARGV[2],ARGV[3],p[1])
    if #claimed>0 then
      local current=redis.call('XPENDING',KEYS[1],ARGV[1],p[1],p[1],1)
      if p[2]~=ARGV[2] and #redis.call('XPENDING',KEYS[1],ARGV[1],'-','+',1,p[2])==0 then redis.pcall('XGROUP','DELCONSUMER',KEYS[1],ARGV[1],p[2]) end
      return {cursor,{claimed[1][1],claimed[1][2],current[1][4]}}
    end
  end
end
if #pending<32 then cursor='-' end
local fresh=redis.call('XREADGROUP','GROUP',ARGV[1],ARGV[2],'COUNT',1,'STREAMS',KEYS[1],'>')
if fresh and #fresh>0 and #fresh[1][2]>0 then
  local m=fresh[1][2][1]
  return {cursor,{m[1],m[2],1}}
end
if #redis.call('XPENDING',KEYS[1],ARGV[1],'-','+',1,ARGV[2])==0 then redis.pcall('XGROUP','DELCONSUMER',KEYS[1],ARGV[1],ARGV[2]) end
return {cursor,false}
