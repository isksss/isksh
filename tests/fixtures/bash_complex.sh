#!/usr/bin/env bash

set -o pipefail

title=${1:-compatibility}
numbers=(3 1 4 1 5)
declare -A labels
labels[3]=odd
labels[4]=even

describe() {
    local value=$1
    local kind

    if [[ $value =~ ^[0-9]+$ && $value -gt 0 ]]; then
        case $((value % 2)) in
            0) kind=even ;;
            *) kind=odd ;;
        esac
    else
        kind=invalid
    fi

    printf '%s:%s' "$value" "$kind"
}

printf 'title=%s\n' "$title"

sum=0
for value in "${numbers[@]}"; do
    sum=$((sum + value))
done
printf 'sum=%d count=%d\n' "$sum" "${#numbers[@]}"
let 'sum+=1'
let 'sum-=1'
printf -v formatted '%s:%d' "$title" "$sum"
builtin printf 'formatted=%s\n' "$formatted"

printf 'items='
separator=
for index in "${!numbers[@]}"; do
    value=${numbers[index]}
    printf '%s%s[%s]=%s' "$separator" "$(describe "$value")" "$index" "${labels[$value]:-unknown}"
    separator=,
done
printf '\n'

joined=$(printf '%s\n' "${numbers[@]}" | sort -n | tr '\n' ':')
printf 'sorted=%s\n' "$joined"

cat <<EOF
heredoc=${title}:$((sum * 2))
EOF

read -r escaped <<'EOF'
a\b
EOF
printf 'raw=%s\n' "$escaped"

mapfile -t rows < <(printf 'alpha\nbeta\ngamma\n')
case ${rows[1]} in
    b*) printf 'selected=%s\n' "${rows[1]}-$(printf '%s' "${rows[1]}" | tr 'a-z' 'A-Z')" ;;
esac

false | true
pipeline_status=$?
pipeline_parts="${PIPESTATUS[*]}"
printf 'pipeline=%s parts=%s\n' "$pipeline_status" "$pipeline_parts"

counter=0
until [[ $counter -ge 3 ]]; do
    counter=$((counter + 1))
done
printf 'counter=%d default=%s\n' "$counter" "${missing:-fallback}"
