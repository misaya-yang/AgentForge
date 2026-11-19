#!/bin/bash
# Test image generation with 3 styles (server-side)
# Run from server: /opt/deploy/ai-gateway/scripts/test_image_styles.sh

set -e
COOKIE=/tmp/img_test_cookie.txt
rm -f "$COOKIE"

BASE_URL="http://127.0.0.1:8080/api/v1/assistant"
OUT_DIR="/tmp/test_images"
mkdir -p "$OUT_DIR"

STYLES=("anime" "watercolor" "sketch")

generate_image() {
    local style=$1
    local idx=$2
    echo "--- Submitting style=$style ---"

    TASK=$(curl -sS -c "$COOKIE" -b "$COOKIE" -X POST "$BASE_URL/generate-image-async" \
        -H "Content-Type: application/json" \
        -d "{\"prompt\":\"一只橘猫\",\"model_id\":\"gemini-3-flash-preview\",\"style\":\"$style\",\"add_watermark\":true,\"n\":1}" \
        | platform-tools json-get - task_id)

    echo "Task: $TASK"

    for i in $(seq 1 40); do
        R=$(curl -sS -c "$COOKIE" -b "$COOKIE" "$BASE_URL/image-task/$TASK")
        S=$(echo "$R" | platform-tools json-get - status)
        echo "  Poll $i: $S"
        if [ "$S" = "completed" ] || [ "$S" = "failed" ]; then
            echo "$R" > "$OUT_DIR/${style}_task.json"
            echo "$R" | platform-tools json-pretty -
            break
        fi
        sleep 3
    done
}

for idx in "${!STYLES[@]}"; do
    generate_image "${STYLES[$idx]}" "$idx"
    echo ""
done

echo "All tasks done. Results in $OUT_DIR"
ls -la "$OUT_DIR"
