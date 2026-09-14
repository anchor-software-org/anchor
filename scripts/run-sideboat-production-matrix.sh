#!/usr/bin/env bash
set -euo pipefail

# Run a small, named encoder/framing matrix. This is deliberately an offline
# matrix: it makes visual and access-unit changes comparable without claiming
# to measure Wi-Fi, QUIC, JNI, or MediaCodec behavior.

output_root="/tmp/anchor-sideboat-production-matrix"
if [[ "${1:-}" != --* && -n "${1:-}" ]]; then
    output_root="$1"
    shift
fi

duration=4
encoder=software
vaapi_device=auto
while (( $# > 0 )); do
    case "$1" in
        --duration)
            duration="${2:?--duration requires seconds}"
            shift 2
            ;;
        --encoder)
            encoder="${2:?--encoder requires software or vaapi}"
            shift 2
            ;;
        --vaapi-device)
            vaapi_device="${2:?--vaapi-device requires a device path or auto}"
            shift 2
            ;;
        *)
            echo "usage: $0 [output-root] [--duration seconds] [--encoder software|vaapi] [--vaapi-device auto|PATH]" >&2
            exit 2
            ;;
    esac
done

case "$encoder" in
    software|vaapi) ;;
    *)
        echo "--encoder must be software or vaapi" >&2
        exit 2
        ;;
esac

mkdir -p "$output_root"
summary="$output_root/matrix.md"
printf '%s\n\n' '# Sideboat production baseline matrix' >"$summary"
printf '%s\n\n' "All profiles use the same deterministic 1920x1080/60 cyberspace scene for ${duration} seconds with the ${encoder} encoder mode." >>"$summary"
printf '%s\n' '| profile | encoder | input | bitrate | GOP | frame cap | decoded/source | mean PSNR | min PSNR | max AU | ANFR fragments |' >>"$summary"
printf '%s\n' '| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |' >>"$summary"

run_profile() {
    local name="$1"
    local bitrate_mbps="$2"
    local gop="$3"
    local max_frame_kb="$4"
    local profile_dir="$output_root/$name"
    local report="$profile_dir/report.json"

    bash scripts/run-sideboat-production-baseline.sh "$profile_dir" \
        --duration "$duration" \
        --bitrate-mbps "$bitrate_mbps" \
        --gop "$gop" \
        --max-frame-kb "$max_frame_kb" \
        --encoder "$encoder" \
        --vaapi-device "$vaapi_device"

    json_number() {
        local key="$1"
        awk -F ': ' -v key="\"$key\"" '$1 ~ key { gsub(/,/, "", $2); gsub(/[[:space:]]/, "", $2); print $2; exit }' "$report"
    }

    local report_encoder input_path source_frames decoded_frames mean_psnr min_psnr max_au fragments
    report_encoder="$(json_number encoder | tr -d '\"')"
    input_path="$(json_number input_path | tr -d '\"')"
    source_frames="$(json_number source_frames)"
    decoded_frames="$(json_number decoded_frames)"
    mean_psnr="$(json_number mean_psnr_db)"
    min_psnr="$(json_number min_psnr_db)"
    max_au="$(json_number max_access_unit_bytes)"
    fragments="$(json_number anfr_fragments)"
    printf '| %s | %s | %s | %s Mbps | %s | %s KiB | %s/%s | %s dB | %s dB | %s B | %s |\n' \
        "$name" "$report_encoder" "$input_path" "$bitrate_mbps" "$gop" "$max_frame_kb" \
        "$decoded_frames" "$source_frames" "$mean_psnr" "$min_psnr" "$max_au" "$fragments" >>"$summary"
}

# These are candidate profiles, not automatically selected application defaults.
# A comparison is meaningful only if all rows decode every source frame.
run_profile constrained 8 120 96
run_profile balanced 15 120 120
run_profile high-detail 25 120 240

printf '\n%s\n' 'Each profile directory contains its own source, decoded, and side-by-side/difference video. Open an individual run with `scripts/run-sideboat-production-baseline.sh <that-directory> --headed` if you want to regenerate and inspect it interactively.' >>"$summary"
printf 'matrix=%s\n' "$summary"
