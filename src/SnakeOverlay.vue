<script setup lang="ts">
import { invoke } from '@tauri-apps/api/core'
import { onMounted, onUnmounted, ref } from 'vue'
import { BODY_RADIUS, HEAD_RADIUS, SnakeGame } from './snake'

const canvas = ref<HTMLCanvasElement | null>(null)
const controls = ref<HTMLElement | null>(null)
const phase = ref<'ready' | 'playing' | 'ended'>('ready')
const score = ref(0)
const notice = ref('')
const closing = ref(false)

let context: CanvasRenderingContext2D | null = null
let game: SnakeGame | null = null
let width = 0
let height = 0
let frame = 0
let previousFrame = 0

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

  const bodyColor = game.crashed ? '#ffab92' : '#9deac0'
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

function sizeCanvas() {
  const element = canvas.value
  if (!element || !context) return
  const nextWidth = element.clientWidth
  const nextHeight = element.clientHeight
  if (width !== nextWidth || height !== nextHeight) {
    stopMovement()
    width = nextWidth
    height = nextHeight
    phase.value = 'ready'
    game = null
    score.value = 0
  }
  const scale = Math.min(window.devicePixelRatio, 2)
  element.width = Math.ceil(width * scale)
  element.height = Math.ceil(height * scale)
  context.setTransform(scale, 0, 0, scale, 0, 0)
  draw()
}

function start() {
  if (!controls.value || !width || !height) return
  stopMovement()
  notice.value = ''
  game = new SnakeGame(width, height, controls.value.getBoundingClientRect())
  phase.value = 'playing'
  score.value = 0
  draw()
}

function animate(now: number) {
  frame = 0
  if (!game || phase.value !== 'playing') return
  const moving = game.advance((now - previousFrame) / 1000)
  previousFrame = now
  score.value = game.score
  draw()
  if (game.crashed) {
    phase.value = 'ended'
  } else if (moving) {
    frame = requestAnimationFrame(animate)
  }
}

function steer(event: PointerEvent) {
  if (!game || phase.value !== 'playing') return
  game.setTarget({ x: event.clientX, y: event.clientY })
  if (!frame) {
    previousFrame = performance.now()
    frame = requestAnimationFrame(animate)
  }
}

async function close() {
  if (closing.value) return
  closing.value = true
  stopMovement()
  try {
    await invoke('close_snake')
  } catch {
    notice.value = '暂时无法关闭游戏，请重试。'
    closing.value = false
  }
}

function onKeydown(event: KeyboardEvent) {
  if (event.repeat) return
  if (event.key === 'Escape') {
    event.preventDefault()
    void close()
  } else if (event.key === 'Enter' && phase.value !== 'playing' && !(event.target instanceof HTMLButtonElement)) {
    event.preventDefault()
    start()
  }
}

onMounted(() => {
  context = canvas.value?.getContext('2d') ?? null
  sizeCanvas()
  window.addEventListener('keydown', onKeydown)
  window.addEventListener('resize', sizeCanvas)
  window.addEventListener('blur', stopMovement)
})

onUnmounted(() => {
  stopMovement()
  window.removeEventListener('keydown', onKeydown)
  window.removeEventListener('resize', sizeCanvas)
  window.removeEventListener('blur', stopMovement)
})
</script>

<template>
  <main>
    <canvas ref="canvas" aria-hidden="true" @pointerenter="steer" @pointermove="steer" @pointerleave="stopMovement"></canvas>
    <section ref="controls" class="hud" aria-labelledby="snake-title">
      <div class="hud-row">
        <h1 id="snake-title">贪吃蛇</h1>
        <span v-if="phase !== 'ready'" class="score">食物 {{ score }}</span>
        <button v-if="phase !== 'playing'" type="button" class="start" @click="start">
          {{ phase === 'ended' ? '再玩一次' : '开始' }}
        </button>
        <button type="button" class="close" :disabled="closing" @click="close">退出 <kbd>Esc</kbd></button>
      </div>
      <p v-if="phase === 'ready'">移动指针引导蛇头，吃下食物会变长。</p>
      <p v-if="phase === 'ended'" role="status">碰到蛇身，游戏结束。</p>
      <p v-if="notice" role="alert">{{ notice }}</p>
    </section>
  </main>
</template>

<style>
:root {
  font-family: -apple-system, BlinkMacSystemFont, "PingFang SC", "Microsoft YaHei", system-ui, sans-serif;
  font-synthesis: none;
  color: #fdf7ef;
  background: transparent;
}

* { box-sizing: border-box; }
body { margin: 0; overflow: hidden; background: transparent; }
canvas {
  position: fixed;
  inset: 0;
  width: 100%;
  height: 100%;
  /* A clear window region can pass pointer events through on macOS. */
  background: rgb(0 0 0 / 2%);
}

.hud {
  position: fixed;
  inset: 16px auto auto 16px;
  max-width: calc(100% - 32px);
  padding: 9px 10px 9px 13px;
  border: 1px solid #62544c;
  border-radius: 11px;
  background: #282322;
  box-shadow: 0 3px 14px #241e1b40;
  user-select: none;
}

.hud-row { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; }
.hud h1 { margin: 0 4px 0 0; font-size: 14px; font-weight: 650; white-space: nowrap; }
.hud p { margin: 7px 0 1px; color: #ddd0c4; font-size: 12px; line-height: 1.55; }
.score { margin-inline-end: 4px; font-size: 12px; font-variant-numeric: tabular-nums; color: #ddd0c4; }

.hud button {
  min-height: 32px;
  padding: 5px 9px;
  border: 1px solid #716059;
  border-radius: 6px;
  color: inherit;
  background: transparent;
  font: inherit;
  font-size: 12px;
  cursor: pointer;
  white-space: nowrap;
}

.hud button:hover { background: #483c37; }
.hud button:focus-visible { outline: 2px solid #ffd5ad; outline-offset: 2px; }
.hud button.start { color: #281d16; background: #ffc999; border-color: #ffc999; font-weight: 650; }
.hud button.start:hover { background: #ffdab8; }
.hud button:disabled { opacity: 0.65; cursor: default; }
kbd { margin-inline-start: 3px; color: #ddd0c4; font: 10px ui-monospace, monospace; }
</style>
