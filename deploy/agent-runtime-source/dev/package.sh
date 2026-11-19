#!/usr/bin/env bash
# Local dev artifact packaging. Never invokes Cargo, changes source, or starts services.
set -euo pipefail
self=$(cd -- "$(dirname -- "$0")" && pwd -P)
fail() { echo "STOP: $*" >&2; exit 3; }
hash() { shasum -a 256 "$1" | awk '{print $1}'; }
mode=${1:---help}
if [[ $mode == --prepare ]]; then
 [[ $# == 8 ]] || fail 'usage: package.sh --prepare EXECUTION_PACKAGE NEW_EXTERNAL_DIR NOTO_CJK_TTC FONT_SHA256 LICENSE LICENSE_SHA256'
 execution=$2; out=$3; font=$4; font_sha=$5; license=$6; license_sha=$7
 # Reserved eighth argument is an explicit local-dev-only confirmation, not approval.
 [[ $8 == local-development-only ]] || fail 'explicit local-development-only scope required'
 repo=${AF_AGENTFORGE_REPO_ROOT:-$(cd "$self/../../.." && pwd -P)}
 [[ -f "$repo/AGENTS.md" && -d "$repo/rust/agent-runtime-overlay" ]] || fail 'repository source root required for preparation'
 [[ $out == /* && $out != "$repo"/* && ! -e $out && ! -L $out ]] || fail 'choose a new absolute external output directory'
 [[ -f $font && ! -L $font && -f $license && ! -L $license && -s $license ]] || fail 'explicit public font and license regular files required'
 [[ $font_sha =~ ^[a-f0-9]{64}$ && $license_sha =~ ^[a-f0-9]{64}$ ]] || fail 'explicit expected public source hashes required'
 [[ $(hash "$font") == "$font_sha" && $(hash "$license") == "$license_sha" ]] || fail 'font/license source checksum mismatch'
 [[ $(od -An -tx1 -N4 "$font" | tr -d ' \n') == 74746366 ]] || fail 'expected genuine TTC font collection'
 bytes=$(wc -c < "$font" | tr -d ' ')
 (( bytes >= 10000000 && bytes <= 33554432 )) || fail 'Noto CJK font outside controlled size range'
 "$self/verify-evidence.sh" "$execution" "$out"
 cp "$self/Dockerfile" "$self/collect-rootfs.sh" "$self/collect-python.sh" "$self/package.sh" "$out/"
 cp "$execution/composed/codex-rs/ai-platform-agent-runtime/container-entrypoint.sh" "$out/"
 # The entrypoint must belong to the actual source unit too.
 expected=$(jq -r '.records[]|select(.path=="ai-platform-agent-runtime/container-entrypoint.sh")|.proposed_sha256' "$out/source-unit.json")
 [[ $expected == "$(hash "$out/container-entrypoint.sh")" ]] || fail 'entrypoint differs from compiled source unit'
 cp "$repo/deploy/agent-runtime-source/python/catalog-arm64.json" "$out/python-catalog.json"
 cp "$execution/composed/LICENSE" "$execution/composed/NOTICE" "$out/"
 mkdir "$out/office"
 cp "$font" "$out/office/NotoSansCJK-Regular.ttc"; cp "$license" "$out/office/LICENSE"
 jq -n --arg sha "$font_sha" --arg license "$license_sha" '{schema:"agentforge/local-dev-office-font/v1",font_sha256:$sha,license_sha256:$license,required_font:"NotoSansCJK-Regular.ttc",source:"explicit-existing-public-local-files",downloaded:false,render_acceptance:false}' > "$out/office/provenance.json"
 # Pin an already-present donor; no daemon requirement for the Rust BuildKit base.
 donor=ai-gateway-agent-runtime:local-092d3acd6bec-12fbb73aec9b
 donor_id=sha256:51ba8952a57c632b5358128f42186f9c636f6d7d349122cd26922a401fe2bde9
 [[ $(docker image inspect "$donor" --format '{{.Id}} {{.Architecture}} {{.Os}}') == "$donor_id arm64 linux" ]] || fail 'existing slim donor identity/platform changed; no pull fallback'
 unit=$(hash "$out/source-unit.json"); receipt=$(hash "$out/built-binaries.json")
 jq -n --arg donor "$donor" --arg id "$donor_id" --arg unit "$unit" --arg receipt "$receipt" --arg execution "$execution" '{schema:"agentforge/local-dev-pair-plan/v1",state:"PREPARED_NOT_BUILT",execution_package:$execution,source_unit_sha256:$unit,binary_receipt_sha256:$receipt,donor:$donor,donor_id:$id,builder:"desktop-linux",platform:"linux/arm64",target_cache_id:"ai-platform-capability-target-rust-1-95-v1",cargo_or_rustc_run:false,production_admitted:false,generated_release_identity:false,release_gates_unchanged:true}' > "$out/plan.json"
 # All selected context bytes frozen; no env, credentials, source tree or registry.
 (cd "$out" && find . -type f ! -name context.sha256 | LC_ALL=C sort | while IFS= read -r file; do shasum -a 256 "$file"; done > context.sha256)
 echo "PREPARED_NOT_BUILT: $out; execute $out/package.sh --build $out under root's existing rust-build lock"
 exit 0
fi
if [[ $mode == --build ]]; then
 [[ $# == 2 || $# == 3 || $# == 5 ]] || fail 'usage: package.sh --build PREPARED_CONTEXT [sandbox-pool|sandbox-attestor|python-sandbox [EXISTING_DONOR EXPECTED_IMAGE_ID]]'
 roles='runtime worker'; selected=${3:-pair}; python_donor=''
 if [[ $selected != pair ]]; then
  [[ $selected == sandbox-pool || $selected == sandbox-attestor || $selected == python-sandbox ]] || fail 'unsupported role'
  roles=$selected
  if [[ $selected == python-sandbox ]]; then
   [[ $# == 5 && $5 =~ ^sha256:[a-f0-9]{64}$ ]] || fail 'explicit already-local Python3.14 donor and expected image ID required'
   [[ $(docker image inspect "$4" --format '{{.Id}} {{.Architecture}} {{.Os}}') == "$5 arm64 linux" ]] || fail 'existing Python donor identity/platform mismatch; never download'
   python_donor=$4
  else [[ $# == 3 ]] || fail 'only Python role accepts a donor'; fi
 else [[ $# == 2 ]] || fail 'pair takes only prepared context'; fi
 context=$(cd "$2" && pwd -P)
 [[ ${AI_PLATFORM_RUST_BUILD_LOCK_HELD:-} == 1 ]] || fail 'primary must hold the existing sole rust-build lock'
 results="${context}-${selected}-results"
 [[ ! -e $results && ! -L $results ]] || fail 'prior attempt exists; preserve it and prepare a new directory'
 (cd "$context" && shasum -a 256 --check --status context.sha256) || fail 'frozen packaging context changed'
 # Reject additions too, especially accidental local-env or credential files.
 expected=$(wc -l < "$context/context.sha256" | tr -d ' ')
 actual=$(find "$context" -type f ! -name context.sha256 | wc -l | tr -d ' ')
 [[ $expected == "$actual" && -z $(find "$context" -type l -print -quit) ]] || fail 'packaging context inventory changed'
 donor=$(jq -r .donor "$context/plan.json"); donor_id=$(jq -r .donor_id "$context/plan.json")
 [[ $(docker image inspect "$donor" --format '{{.Id}} {{.Architecture}} {{.Os}}') == "$donor_id arm64 linux" ]] || fail 'immutable local donor mismatch'
 builder_state=$(docker buildx inspect desktop-linux)
 [[ $(printf '%s\n' "$builder_state" | awk '$1=="Name:"{print $2;exit}') == desktop-linux && $(printf '%s\n' "$builder_state" | awk '$1=="Driver:"{print $2;exit}') == docker ]] || fail 'existing desktop-linux docker builder required' 
 unit=$(jq -r .source_unit_sha256 "$context/plan.json"); receipt=$(jq -r .binary_receipt_sha256 "$context/plan.json")
 [[ $unit == "$(hash "$context/source-unit.json")" && $receipt == "$(hash "$context/built-binaries.json")" ]] || fail 'planned artifact identity changed'
 # Readonly target cache; no Cargo graph, target, registry mount, cache export or seed.
 # Dynamic budget accounts for all ELF copies/stages/load, with a 10GiB reserve.
 binary_bytes=$(jq '[.binaries[].bytes]|add' "$context/built-binaries.json")
 growth_kib=$(( (binary_bytes * 5 + 1073741824 + 1023) / 1024 ))
 (( growth_kib <= 8 * 1024 * 1024 )) || fail 'actual binary sizes exceed reviewed 8GiB packaging budget'
 free_kib=$(df -Pk "$context" | awk 'NR==2 {print $4}')
 [[ $free_kib =~ ^[0-9]+$ ]] && (( free_kib >= growth_kib + 10 * 1024 * 1024 )) || fail 'insufficient free space for actual artifact-size budget plus10GiB reserve'
 stop_kib=$((free_kib-growth_kib))
 mkdir -m 0700 "$results"
 nonce=$(date -u +%Y%m%dT%H%M%SZ)-$$
 jq -n --argjson free "$free_kib" --argjson growth "$growth_kib" --argjson stop "$stop_kib" --arg nonce "$nonce" '{baseline_free_kib:$free,max_growth_kib:$growth,stop_free_kib:$stop,timeout_seconds:1800,nonce:$nonce,hard_disk_quota:false}' > "$results/budget.json"
 build_pid=''; monitor_pid=''
 cleanup() { if [[ -n $monitor_pid ]]; then kill "$monitor_pid" 2>/dev/null || true; fi; if [[ -n $build_pid ]]; then kill -TERM "$build_pid" 2>/dev/null || true; fi; }
 trap cleanup EXIT HUP INT TERM
 for role in $roles; do
  extra=(); [[ -z $python_donor ]] || extra+=(--build-arg "PYTHON_DONOR=$python_donor")
  tag="agentforge-execution-dev:${role}-${unit:0:12}-${receipt:0:12}"
  docker buildx build --builder desktop-linux --platform linux/arm64 --pull=false --network=none --progress=plain --load \
   --build-arg "RUNTIME_DONOR=$donor" --build-arg "VERIFY_NONCE=$nonce" \
   --build-arg "SOURCE_UNIT_SHA256=$unit" --build-arg "BINARY_RECEIPT_SHA256=$receipt" \
   "${extra[@]}" --target "$role" --tag "$tag" --metadata-file "$results/$role-build.json" \
   -f "$context/Dockerfile" "$context" > "$results/$role.log" 2>&1 &
  build_pid=$!
  (
   started=$SECONDS
   while kill -0 "$build_pid" 2>/dev/null; do
    free=$(df -Pk "$context" | awk 'NR==2 {print $4}')
    if [[ ! $free =~ ^[0-9]+$ ]] || (( free <= stop_kib || SECONDS-started >= 1800 )); then
     echo 'actual disk/time budget stop' > "$results/budget-stop.txt"; kill -TERM "$build_pid" 2>/dev/null || true; exit 1
    fi
    sleep 1
   done
  ) & monitor_pid=$!
  code=0; wait "$build_pid" || code=$?; build_pid=''
  kill "$monitor_pid" 2>/dev/null || true; wait "$monitor_pid" 2>/dev/null || true; monitor_pid=''
  [[ ! -e $results/budget-stop.txt ]] && (( code == 0 )) || fail "actual $role image build failed; preserved results"
  docker image inspect "$tag" --format '{{json .}}' | jq '{Id,Architecture,Os,Size,Config:{User:.Config.User,Entrypoint:.Config.Entrypoint,WorkingDir:.Config.WorkingDir,Labels:.Config.Labels}}' > "$results/$role-image.json"
  jq -e --arg unit "$unit" --arg receipt "$receipt" '.Architecture=="arm64" and .Os=="linux" and .Config.User=="10001:10001" and .Config.Labels["agentforge.execution.source-unit-sha256"]==$unit and .Config.Labels["agentforge.execution.binary-receipt-sha256"]==$receipt and .Config.Labels["agentforge.execution.production-admitted"]=="false" and .Config.Labels["agentforge.execution.generated-release-identity"]=="false" and (.Config.Labels|keys|all(startswith("com.misaya")|not))' "$results/$role-image.json" >/dev/null || fail 'actual dev image identity guard failed'
  # Real non-service smoke, no mounts, no network/provider/DB calls.
  binary=ai-platform-agent-runtime
  case "$role" in worker) binary=ai-platform-capability-worker;; python-sandbox) binary=ai-platform-python-sandbox;; sandbox-pool|sandbox-attestor) binary="ai-platform-$role";; esac
  if [[ $role == runtime || $role == worker ]]; then
   version_mounts=(--tmpfs /tmp:rw,noexec,nosuid,nodev,size=16m,mode=1777)
   if [[ $role == runtime ]]; then
    # Upstream arg0 creates temporary helper aliases before Clap --version exits.
    # Keep those ephemeral so the first real home initialization stays empty.
    version_mounts+=(--tmpfs /runtime-home:rw,noexec,nosuid,nodev,size=16m,mode=0700,uid=10001,gid=10001 --tmpfs /var/lib/ai-platform-agent-runtime:rw,noexec,nosuid,nodev,size=16m,mode=0700,uid=10001,gid=10001)
   fi
   docker run --rm --pull=never --network=none --read-only "${version_mounts[@]}" --cap-drop=ALL --security-opt=no-new-privileges --memory=256m --cpus=1 --pids-limit=32 --entrypoint="/usr/local/bin/$binary" "$tag" --version > "$results/$role-version.txt" 2>&1
  else
   # Role entrypoints require real config, and have no --version CLI. Hash-only
   # smoke cannot start controller/attestor/user-code or claim attestation.
   docker run --rm --pull=never --network=none --read-only --cap-drop=ALL --security-opt=no-new-privileges --memory=256m --cpus=1 --pids-limit=32 --entrypoint=/usr/bin/sha256sum "$tag" "/usr/local/bin/$binary" > "$results/$role-binary.sha256"
   expected_binary=$(jq -r --arg name "$binary" '.binaries[]|select(.name==$name)|.sha256' "$context/built-binaries.json")
   [[ $(awk '{print $1}' "$results/$role-binary.sha256") == "$expected_binary" ]] || fail 'optional role actual ELF hash mismatch'
  fi
  if [[ $role == runtime || $role == worker ]]; then
   docker run --rm --pull=never --network=none --read-only --cap-drop=ALL --security-opt=no-new-privileges --memory=256m --cpus=1 --pids-limit=32 --entrypoint=/bin/sh "$tag" -ec '
    test "$(id -u)" = 10001
    test ! -e /source; test ! -e /cargo-target; test ! -e /usr/local/cargo; test ! -e /usr/local/rustup
    test ! -e /usr/bin/rustc; test ! -e /usr/bin/cargo; test ! -e /usr/bin/python3; test ! -e /etc/ssl/private
    for file in /usr/local/bin/ai-platform-*; do sha256sum "$file"; done
    cat /usr/share/agentforge/dev/elf-linkage.txt >&2
   ' > "$results/$role-actual-binaries.sha256" 2> "$results/$role-elf-linkage.txt"
   while read -r actual_hash executable; do
    name=${executable##*/}
    if [[ $name == ai-platform-runtime-entrypoint ]]; then
     [[ $actual_hash == "$(hash "$context/container-entrypoint.sh")" ]] || fail 'actual platform entrypoint hash mismatch'
     continue
    fi
    wanted=$(jq -r --arg name "$name" '.binaries[]|select(.name==$name)|.sha256' "$context/built-binaries.json")
    [[ -n $wanted && $actual_hash == "$wanted" ]] || fail 'actual final image contains an unbound executable'
   done < "$results/$role-actual-binaries.sha256"
   expected_count=3; [[ $role == runtime ]] || expected_count=6
   [[ $(wc -l < "$results/$role-actual-binaries.sha256" | tr -d ' ') == "$expected_count" ]] || fail 'actual final image executable scope changed'
   if [[ $role == runtime ]]; then
    docker run --rm --pull=never --network=none --read-only --cap-drop=ALL --security-opt=no-new-privileges --memory=128m --cpus=1 --pids-limit=16 --entrypoint=/bin/sh "$tag" -ec 'test -z "$(find /runtime-home -mindepth 1 -print -quit)"; test ! -e /runtime-home/.ai-platform-runtime-home' > "$results/runtime-first-home-empty.txt" 2>&1
    docker run --rm --pull=never --network=none --read-only --tmpfs /tmp:rw,nosuid,nodev,size=16m,mode=1777 --cap-drop=ALL --security-opt=no-new-privileges --memory=256m --cpus=1 --pids-limit=32 --entrypoint=/bin/bash "$tag" -euc '
     sample=$(mktemp -d /tmp/agentforge-dev-smoke.XXXXXX)
     git init -q "$sample"
     printf "source-bound smoke\n" > "$sample/probe.txt"
     git -C "$sample" add probe.txt
     git -C "$sample" -c user.name=AgentForgeSmoke -c user.email=smoke@example.invalid commit -qm smoke --no-gpg-sign --no-verify
     rg -q source-bound "$sample/probe.txt"
     git -C "$sample" status --porcelain
     bwrap --version
    ' > "$results/runtime-shell-git-smoke.txt" 2>&1
   fi
   if [[ $role == worker ]]; then
    docker run --rm --pull=never --network=none --read-only --cap-drop=ALL --security-opt=no-new-privileges --memory=256m --cpus=1 --pids-limit=32 --entrypoint=/bin/sh "$tag" -ec 'test ! -e /usr/local/bin/ai-platform-python-supervisor; test ! -e /usr/local/bin/ai-platform-python-sandbox; sha256sum /usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc' > "$results/worker-office-font.sha256"
    [[ $(awk '{print $1}' "$results/worker-office-font.sha256") == "$(jq -r .font_sha256 "$context/office/provenance.json")" ]] || fail 'actual CJK font bytes mismatch'
   fi
  fi
  printf '%s\n' "$tag" > "$results/$role-tag.txt"
 done
 if [[ $selected == pair ]]; then
 jq -n --arg unit "$unit" --arg receipt "$receipt" --slurpfile runtime "$results/runtime-image.json" --slurpfile worker "$results/worker-image.json" '{schema:"agentforge/local-dev-pair-built/v1",success:true,source_unit_sha256:$unit,binary_receipt_sha256:$receipt,runtime:$runtime[0],worker:$worker[0],production_admitted:false,generated_release_identity:false,services_started:false,cargo_or_rustc_run:false}' > "$results/paired-images.json"
 echo "Actual paired development image identities: $results/paired-images.json; services remain stopped"
 else echo "Actual optional role image identity: $results/$selected-image.json; workload isolation/attestation acceptance remains unrun"; fi
 exit 0
fi
echo 'usage: package.sh --prepare EXECUTION_PACKAGE NEW_EXTERNAL_DIR NOTO_CJK_TTC FONT_SHA256 LICENSE LICENSE_SHA256 local-development-only | --build PREPARED_CONTEXT'
