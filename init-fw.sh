#!/bin/sh
set -ev
json=""
while IFS= read -r line; do
    json+="$line"
    [[ "$line" == *"}"* ]] && break
done

echo $json
pid=$(jq -r '.["child-pid"]' <<< "$json")
echo PID=$pid
#netns=$(jq -r '.["net-namespace"]' <<< "$json")
#strace -ff pasta --config-net --netns-only "$pid"
#strace -ff pasta --config-net --netns "$netns" --netns-only

read subuid_start subuid_count < <(awk -F: '$1=="'$USER'" {print $2, $3}' /etc/subuid)
read subgid_start subgid_count < <(awk -F: '$1=="'$USER'" {print $2, $3}' /etc/subgid)
echo $subuid_start - $subuid_count
newuidmap "$pid" \
  0 "$subuid_start" "$((subuid_count))"
#  0 "$(id -u)" 1 \
newgidmap "$pid" \
  0 "$(id -g)" 1 \
  1 "$subgid_start" "$subgid_count"
