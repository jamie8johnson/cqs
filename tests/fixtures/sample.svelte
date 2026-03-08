<script>
  let count = 0;

  function increment() {
    count += 1;
  }

  function reset() {
    count = 0;
  }
</script>

<button on:click={increment}>
  Count: {count}
</button>
