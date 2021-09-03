package rsync_test

import (
	"bytes"
	"io/ioutil"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"testing"

	"github.com/gokrazy/rsync/internal/rsynctest"
	"github.com/google/go-cmp/cmp"
	"golang.org/x/sys/unix"
)

// TODO: non-empty exclusion list

func TestModuleListing(t *testing.T) {
	tmp := t.TempDir()

	// start a server to sync from
	srv := rsynctest.New(t, rsynctest.InteropModMap(tmp))

	// request module list
	var buf bytes.Buffer
	rsync := exec.Command("rsync", //"/home/michael/src/openrsync/openrsync",
		//		"--debug=all4",
		"--archive",
		"-v", "-v", "-v", "-v",
		"--port="+srv.Port,
		"rsync://localhost")
	rsync.Stdout = &buf
	rsync.Stderr = os.Stderr
	if err := rsync.Run(); err != nil {
		t.Fatalf("%v: %v", rsync.Args, err)
	}

	output := buf.String()
	if want := "interop\tinterop"; !strings.Contains(output, want) {
		t.Fatalf("rsync output unexpectedly did not contain %q:\n%s", want, output)
	}
}

func TestInterop(t *testing.T) {
	tmp := t.TempDir()
	source := filepath.Join(tmp, "source")
	dest := filepath.Join(tmp, "dest")

	// create files in source to be copied
	if err := os.MkdirAll(source, 0755); err != nil {
		t.Fatal(err)
	}
	dummy := filepath.Join(source, "dummy")
	want := []byte("heyo")
	if err := ioutil.WriteFile(dummy, want, 0644); err != nil {
		t.Fatal(err)
	}

	linkToDummy := filepath.Join(source, "link_to_dummy")
	if err := os.Symlink("dummy", linkToDummy); err != nil {
		t.Fatal(err)
	}

	if os.Getuid() == 0 {
		char := filepath.Join(source, "char")
		// major 1, minor 5, like /dev/zero
		if err := unix.Mknod(char, 0600|syscall.S_IFCHR, int(unix.Mkdev(1, 5))); err != nil {
			t.Fatal(err)
		}
		block := filepath.Join(source, "block")
		// major 242, minor 9, like /dev/nvme0
		if err := unix.Mknod(block, 0600|syscall.S_IFBLK, int(unix.Mkdev(242, 9))); err != nil {
			t.Fatal(err)
		}

		fifo := filepath.Join(source, "fifo")
		if err := unix.Mkfifo(fifo, 0600); err != nil {
			t.Fatal(err)
		}

		sock := filepath.Join(source, "sock")
		ln, err := net.Listen("unix", sock)
		if err != nil {
			t.Fatal(err)
		}
		t.Cleanup(func() { ln.Close() })
	}

	// start a server to sync from
	srv := rsynctest.New(t, rsynctest.InteropModMap(source))

	// 	{
	// 		config := filepath.Join(tmp, "rsyncd.conf")
	// 		rsyncdConfig := `
	// 	use chroot = no
	// 	# 0 = no limit
	// 	max connections = 0
	// 	pid file = ` + tmp + `/rsyncd.pid
	// 	exclude = lost+found/
	// 	transfer logging = yes
	// 	timeout = 900
	// 	ignore nonreadable = yes
	// 	dont compress   = *.gz *.tgz *.zip *.z *.Z *.rpm *.deb *.bz2 *.zst

	// 	[interop]
	// 	       path = /home/michael/i3/docs
	// #` + source + `
	// 	       comment = interop
	// 	       read only = yes
	// 	       list = true

	// 	`
	// 		if err := ioutil.WriteFile(config, []byte(rsyncdConfig), 0644); err != nil {
	// 			t.Fatal(err)
	// 		}
	// 		srv := exec.Command("rsync",
	// 			"--daemon",
	// 			"--config="+config,
	// 			"--verbose",
	// 			"--address=localhost",
	// 			"--no-detach",
	// 			"--port=8730")
	// 		srv.Stdout = os.Stdout
	// 		srv.Stderr = os.Stderr
	// 		if err := srv.Start(); err != nil {
	// 			t.Fatal(err)
	// 		}
	// 		go func() {
	// 			if err := srv.Wait(); err != nil {
	// 				t.Error(err)
	// 			}
	// 		}()
	// 		defer srv.Process.Kill()
	//
	//      time.Sleep(1 * time.Second)
	// 	}

	rsync := exec.Command("rsync", //"/home/michael/src/openrsync/openrsync",
		"--version")
	rsync.Stdout = os.Stdout
	rsync.Stderr = os.Stderr
	if err := rsync.Run(); err != nil {
		t.Fatalf("%v: %v", rsync.Args, err)
	}

	// dry run (slight differences in protocol)
	rsync = exec.Command("rsync", //"/home/michael/src/openrsync/openrsync",
		//		"--debug=all4",
		"--archive",
		"-v", "-v", "-v", "-v",
		"--port="+srv.Port,
		"--dry-run",
		"rsync://localhost/interop/", // copy contents of interop
		//source+"/", // sync from local directory
		dest) // directly into dest
	rsync.Stdout = os.Stdout
	rsync.Stderr = os.Stderr
	if err := rsync.Run(); err != nil {
		t.Fatalf("%v: %v", rsync.Args, err)
	}

	// sync into dest dir
	rsync = exec.Command("rsync", //"/home/michael/src/openrsync/openrsync",
		//		"--debug=all4",
		"--archive",
		"-v", "-v", "-v", "-v",
		"--port="+srv.Port,
		"rsync://localhost/interop/", // copy contents of interop
		//source+"/", // sync from local directory
		dest) // directly into dest
	rsync.Stdout = os.Stdout
	rsync.Stderr = os.Stderr
	if err := rsync.Run(); err != nil {
		t.Fatalf("%v: %v", rsync.Args, err)
	}

	{
		got, err := ioutil.ReadFile(filepath.Join(dest, "dummy"))
		if err != nil {
			t.Fatal(err)
		}
		if diff := cmp.Diff(want, got); diff != "" {
			t.Fatalf("unexpected file contents: diff (-want +got):\n%s", diff)
		}
	}

	{
		got, err := os.Readlink(filepath.Join(dest, "link_to_dummy"))
		if err != nil {
			t.Fatal(err)
		}
		if want := "dummy"; got != want {
			t.Fatalf("unexpected symlink target: got %q, want %q", got, want)
		}
	}

	if os.Getuid() == 0 {
		{
			st, err := os.Stat(filepath.Join(dest, "char"))
			if err != nil {
				t.Fatal(err)
			}
			if st.Mode().Type()&os.ModeCharDevice == 0 {
				t.Fatalf("unexpected type: got %v, want character device", st.Mode())
			}
			sys, ok := st.Sys().(*syscall.Stat_t)
			if !ok {
				t.Fatal("stat does not contain rdev")
			}
			if got, want := sys.Rdev, unix.Mkdev(1, 5); got != want {
				t.Fatalf("unexpected rdev: got %v, want %v", got, want)
			}
		}

		{
			st, err := os.Stat(filepath.Join(dest, "block"))
			if err != nil {
				t.Fatal(err)
			}
			if st.Mode().Type()&os.ModeDevice == 0 ||
				st.Mode().Type()&os.ModeCharDevice != 0 {
				t.Fatalf("unexpected type: got %v, want block device", st.Mode())
			}
			sys, ok := st.Sys().(*syscall.Stat_t)
			if !ok {
				t.Fatal("stat does not contain rdev")
			}
			if got, want := sys.Rdev, unix.Mkdev(242, 9); got != want {
				t.Fatalf("unexpected rdev: got %v, want %v", got, want)
			}
		}

		{
			st, err := os.Stat(filepath.Join(dest, "fifo"))
			if err != nil {
				t.Fatal(err)
			}
			if st.Mode().Type()&os.ModeNamedPipe == 0 {
				t.Fatalf("unexpected type: got %v, want fifo", st.Mode())
			}
		}

		{
			st, err := os.Stat(filepath.Join(dest, "sock"))
			if err != nil {
				t.Fatal(err)
			}
			if st.Mode().Type()&os.ModeSocket == 0 {
				t.Fatalf("unexpected type: got %v, want socket", st.Mode())
			}
		}
	}

	// Run rsync again. This should not modify any files, but will result in
	// rsync sending sums to the sender.
	rsync = exec.Command("rsync", //"/home/michael/src/openrsync/openrsync",
		//		"--debug=all4",
		"--archive",
		// TODO: should this be --checksum instead?
		"--ignore-times", // disable rsync’s “quick check”
		"-v", "-v", "-v", "-v",
		"--port="+srv.Port,
		"rsync://localhost/interop/", // copy contents of interop
		//source+"/", // sync from local directory
		dest) // directly into dest
	rsync.Stdout = os.Stdout
	rsync.Stderr = os.Stderr
	if err := rsync.Run(); err != nil {
		t.Fatalf("%v: %v", rsync.Args, err)
	}

}
