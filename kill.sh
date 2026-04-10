#kill -9 $(ps aux | grep "$USER" | grep /run/current-system/systemd/lib/systemd/systemd | head -n1|awk '{print $2}')
kill -SIGRTMIN+3 $(ps aux | grep "$USER" | grep /run/current-system/systemd/lib/systemd/systemd | head -n1|awk '{print $2}')
