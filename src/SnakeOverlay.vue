<script setup lang="ts">
import { invoke, isTauri } from '@tauri-apps/api/core'
import { onMounted, onUnmounted, ref } from 'vue'
import { BODY_RADIUS, HEAD_RADIUS, SnakeGame } from './snake'

type Cursor = [number, number]

interface PointerSample {
  point: Cursor | null
  restart: number
}

const POLL_INTERVAL = 1000 / 30
const canvas = ref<HTMLCanvasElement | null>(null)

let context: CanvasRenderingContext2D | null = null
let game: SnakeGame | null = null
let width = 0
let height = 0
let frame = 0
let previousFrame = 0
let cursor: Cursor | null = null
let restart = 0
let polling = false
let pollTimer: number | undefined

function disk(ctx: CanvasRenderingContext2D, x: number, y: number, radius: number, color: string) {
  ctx.beginPath()
  ctx.arc(x, y, radius, 0, 2 * Math.PI)
  ctx.fillStyle = color
  ctx.fill()
}

function draw() {
  const ctx = context
  if (!ctx) return
  ctx.clearRect(0, 0, width, height)
  if (!game) return

  for (const { x, y, radius, color } of game.food) {
    disk(ctx, x, y, radius + 2, '#292022')
    disk(ctx, x, y, radius, color)
    disk(ctx, x - radius * 0.26, y - radius * 0.28, 2, '#fff1d9')
  }

  const bodyColor = '#9deac0'
  const tail = game.trail[0]
  ctx.beginPath()
  ctx.moveTo(tail.x, tail.y)
  for (let i = 1; i < game.trail.length; i++) {
    const point = game.trail[i]
    ctx.lineTo(point.x, point.y)
  }
  ctx.lineCap = 'round'
  ctx.lineJoin = 'round'
  ctx.lineWidth = 2 * (BODY_RADIUS + 2)
  ctx.strokeStyle = '#20322e'
  ctx.stroke()
  ctx.lineWidth = 2 * BODY_RADIUS
  ctx.strokeStyle = bodyColor
  ctx.stroke()

  const { x, y } = game.head
  disk(ctx, x, y, HEAD_RADIUS + 2, '#20322e')
  disk(ctx, x, y, HEAD_RADIUS, bodyColor)
  ctx.save()
  ctx.translate(x, y)
  ctx.rotate(game.heading)
  disk(ctx, 3, -4, 1.65, '#20322e')
  disk(ctx, 3, 4, 1.65, '#20322e')
  ctx.restore()
}

function stopMovement() {
  game?.setTarget(null)
  if (frame) cancelAnimationFrame(frame)
  frame = 0
}

function resetGame() {
  stopMovement()
  cursor = null
  game = width && height ? new SnakeGame(width, height) : null
  draw()
}

function sizeCanvas() {
  const element = canvas.value
  if (!element || !context) return
  const nextWidth = element.clientWidth
  const nextHeight = element.clientHeight
  const resized = width !== nextWidth || height !== nextHeight
  width = nextWidth
  height = nextHeight
  const scale = Math.min(window.devicePixelRatio, 2)
  element.width = Math.ceil(width * scale)
  element.height = Math.ceil(height * scale)
  context.setTransform(scale, 0, 0, scale, 0, 0)
  if (resized) resetGame()
  else draw()
}

function animate(now: number) {
  frame = 0
  if (!game) return
  const moving = game.advance((now - previousFrame) / 1000)
  previousFrame = now
  draw()
  if (moving) frame = requestAnimationFrame(animate)
}

function steer(point: Cursor | null) {
  if (point?.[0] === cursor?.[0] && point?.[1] === cursor?.[1]) return
  cursor = point
  if (!point) {
    stopMovement()
    return
  }
  if (!game) return
  game.setTarget({ x: point[0], y: point[1] })
  if (!frame) {
    previousFrame = performance.now()
    frame = requestAnimationFrame(animate)
  }
}

async function pollPointer() {
  const started = performance.now()
  let sample: PointerSample | null = null
  try {
    sample = await invoke<PointerSample | null>('snake_pointer')
  } catch {
    // Treat a missed sample as a pointer outside the play area.
  }
  if (!polling) return
  if (sample && sample.restart !== restart) {
    restart = sample.restart
    resetGame()
  }
  steer(sample?.point ?? null)
  // Wait for each native reply before scheduling another read.
  const interval = sample ? POLL_INTERVAL : 100
  pollTimer = window.setTimeout(pollPointer, Math.max(0, Math.ceil(interval - (performance.now() - started))))
}

function stopPolling() {
  polling = false
  window.clearTimeout(pollTimer)
  stopMovement()
}

onMounted(() => {
  context = canvas.value?.getContext('2d') ?? null
  if (!context) return
  sizeCanvas()
  window.addEventListener('resize', sizeCanvas)
  if (!isTauri()) return
  window.addEventListener('pagehide', stopPolling)
  polling = true
  void pollPointer()
})

onUnmounted(() => {
  stopPolling()
  window.removeEventListener('resize', sizeCanvas)
  window.removeEventListener('pagehide', stopPolling)
})
</script>

<template>
  <canvas ref="canvas" aria-hidden="true"></canvas>
</template>

<style>
html, body { margin: 0; overflow: hidden; background: transparent; }
canvas {
  position: fixed;
  inset: 0;
  width: 100%;
  height: 100%;
  pointer-events: none;
}
</style>
