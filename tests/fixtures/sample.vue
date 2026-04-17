<template>
  <div class="counter">
    <p>Count: {{ count }}</p>
    <button @click="increment">Increment</button>
  </div>
</template>

<script>
export default {
  data() {
    return { count: 0 }
  },
  methods: {
    increment() {
      this.count++
    },
    reset() {
      this.count = 0
    }
  }
}
</script>

<style scoped>
.counter { padding: 10px; }
</style>
