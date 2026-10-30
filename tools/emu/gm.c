// gm: the memory of the running Astris process, where the Switch game's code and RAM live natively (emu.py
// builds it into build/emu/gm, ad-hoc signed with gm.entitlements: task_for_pid needs the debugger entitlement).
//
//     gm find HEX [MAX]        addresses of a byte pattern in the readable regions (default 50)
//     gm read ADDR LEN         the bytes, raw, to stdout
//     gm write ADDR HEX        poke bytes
//     gm regions               each region: address, size, protection
#include <libproc.h>
#include <mach/mach.h>
#include <mach/mach_vm.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int astris_pid(void) {
    int pids[4096];
    int n = proc_listpids(PROC_ALL_PIDS, 0, pids, sizeof pids) / (int)sizeof(int);
    for (int i = 0; i < n; i++) {
        char p[PROC_PIDPATHINFO_MAXSIZE];
        if (proc_pidpath(pids[i], p, sizeof p) > 0 && strstr(p, "Astris.app/Contents/MacOS/")) return pids[i];
    }
    return -1;
}

static int unhex(const char *s, unsigned char *o, int max) {
    int n = 0;
    while (s[0] && s[1] && n < max) {
        sscanf(s, "%2hhx", &o[n++]);
        s += 2;
    }
    return n;
}

static int region(task_t t, mach_vm_address_t *a, mach_vm_size_t *size, vm_prot_t *prot) {
    vm_region_basic_info_data_64_t info;
    mach_msg_type_number_t count = VM_REGION_BASIC_INFO_COUNT_64;
    mach_port_t object;
    if (mach_vm_region(t, a, size, VM_REGION_BASIC_INFO_64, (vm_region_info_t)&info, &count, &object)) return 0;
    *prot = info.protection;
    return 1;
}

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "gm find HEX [MAX] | read ADDR LEN | write ADDR HEX | regions\n");
        return 2;
    }
    int pid = astris_pid();
    if (pid < 0) {
        fprintf(stderr, "Astris is not running\n");
        return 1;
    }
    task_t t;
    kern_return_t kr = task_for_pid(mach_task_self(), pid, &t);
    if (kr) {
        fprintf(stderr, "task_for_pid %d: %s\n", pid, mach_error_string(kr));
        return 1;
    }
    const char *op = argv[1];
    if (!strcmp(op, "find") && argc > 2) {
        unsigned char pat[4096];
        int pn = unhex(argv[2], pat, sizeof pat);
        int max = argc > 3 ? atoi(argv[3]) : 50;
        mach_vm_address_t a = 0;
        mach_vm_size_t size;
        vm_prot_t prot;
        // in 16 MiB chunks, each read with the pattern's length past its end so a match across chunks is found
        const mach_vm_size_t chunk = 1 << 24;
        for (; region(t, &a, &size, &prot); a += size) {
            if (!(prot & VM_PROT_READ) || size > (1ull << 34)) continue;
            for (mach_vm_size_t off = 0; off < size; off += chunk) {
                mach_vm_size_t len = size - off < chunk ? size - off : chunk;
                vm_offset_t d;
                mach_msg_type_number_t dn;
                if (mach_vm_read(t, a + off, len + (off + len < size ? pn : 0), &d, &dn)
                    && mach_vm_read(t, a + off, len, &d, &dn))
                    continue;
                unsigned char *p = (unsigned char *)d;
                for (mach_msg_type_number_t i = 0; i + pn <= dn && i < len; i++) {
                    if (p[i] == pat[0] && !memcmp(p + i, pat, pn)) {
                        printf("0x%llx\n", a + off + i);
                        if (--max == 0) return 0;
                    }
                }
                vm_deallocate(mach_task_self(), d, dn);
            }
        }
        return 0;
    }
    if (!strcmp(op, "read") && argc > 3) {
        vm_offset_t d;
        mach_msg_type_number_t dn;
        if (mach_vm_read(t, strtoull(argv[2], 0, 0), strtoull(argv[3], 0, 0), &d, &dn)) {
            fprintf(stderr, "read failed\n");
            return 1;
        }
        fwrite((void *)d, 1, dn, stdout);
        return 0;
    }
    if (!strcmp(op, "write") && argc > 3) {
        unsigned char b[4096];
        int n = unhex(argv[3], b, sizeof b);
        kr = mach_vm_write(t, strtoull(argv[2], 0, 0), (vm_offset_t)b, n);
        if (kr) {
            fprintf(stderr, "write: %s\n", mach_error_string(kr));
            return 1;
        }
        return 0;
    }
    if (!strcmp(op, "regions")) {
        mach_vm_address_t a = 0;
        mach_vm_size_t size;
        vm_prot_t prot;
        for (; region(t, &a, &size, &prot); a += size) printf("0x%llx 0x%llx %d\n", a, size, prot);
        return 0;
    }
    fprintf(stderr, "gm find HEX [MAX] | read ADDR LEN | write ADDR HEX | regions\n");
    return 2;
}
