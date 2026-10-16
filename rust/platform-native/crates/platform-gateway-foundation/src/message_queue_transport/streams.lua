-- Shared helpers. Never trim an unconsumed or pending entry in any consumer group.
local function before(a,b)
  local am,as = string.match(a,'^(%d+)%-(%d+)$')
  local bm,bs = string.match(b,'^(%d+)%-(%d+)$')
  if #am ~= #bm then return #am < #bm end
  if am ~= bm then return am < bm end
  if #as ~= #bs then return #as < #bs end
  return as < bs
end
local function trim_safe(key)
  if redis.call('EXISTS',key)==0 then return end
  local groups=redis.call('XINFO','GROUPS',key)
  if #groups==0 then return end
  local floor=nil
  for _,g in ipairs(groups) do
    local name,last
    for i=1,#g,2 do
      if g[i]=='name' then name=g[i+1] end
      if g[i]=='last-delivered-id' then last=g[i+1] end
    end
    local pending=redis.call('XPENDING',key,name,'-','+',1)
    if #pending>0 and before(pending[1][1],last) then last=pending[1][1] end
    if not floor or before(last,floor) then floor=last end
  end
  if floor then
    redis.call('XTRIM',key,'MINID','=',floor)
    -- floor itself is consumable history only when no group still owns it.
    -- Every group's delivery watermark is >= floor by construction.
    local retain=false
    for _,g in ipairs(groups) do
      local name
      for i=1,#g,2 do if g[i]=='name' then name=g[i+1] end end
      if #redis.call('XPENDING',key,name,floor,floor,1)>0 then retain=true end
    end
    if not retain then redis.call('XDEL',key,floor) end
  end
end
