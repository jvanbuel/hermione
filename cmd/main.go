package main

import (
	"fmt"
	"os"
	"os/exec"
	"time"
)

func main() {

	cmd := exec.Command("zsh")
	go func() {

		cmd.Stdin = os.Stdin
		cmd.Stdout = os.Stdout
		cmd.Stderr = os.Stderr
		cmd.Run()
	}()

	for {
		time.Sleep(5 * time.Second)
		cmd.Stdout.Write([]byte(fmt.Sprintf("pid: %d\n", cmd.Process.Pid)))
	}
}
