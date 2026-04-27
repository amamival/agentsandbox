#include <errno.h>
#include <seccomp.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#ifndef __NR_syscalls
#define __NR_syscalls 1024
#endif

int main(void) {
  int rc;
  int nr;
  char *name;
  scmp_filter_ctx ctx = seccomp_init(SCMP_ACT_KILL_PROCESS);

  if (!ctx) {
    fprintf(stderr, "seccomp_init: %s\n", strerror(errno));
    return 1;
  }
  for (nr = 0; nr < __NR_syscalls; nr++) {
    name = seccomp_syscall_resolve_num_arch(SCMP_ARCH_NATIVE, nr);
    if (!name) continue;
    rc = seccomp_rule_add(ctx, SCMP_ACT_ALLOW, nr, 0);
    free(name);
    if (rc < 0) {
      fprintf(stderr, "seccomp_rule_add(%d): %s\n", nr, strerror(-rc));
      seccomp_release(ctx);
      return 1;
    }
  }
  rc = seccomp_export_bpf(ctx, STDOUT_FILENO);
  seccomp_release(ctx);
  if (rc < 0) {
    fprintf(stderr, "seccomp_export_bpf: %s\n", strerror(-rc));
    return 1;
  }
  return 0;
}
