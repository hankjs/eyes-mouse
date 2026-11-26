<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref } from 'vue'
import { Channel, invoke } from '@tauri-apps/api/core'

type Point = [number, number]
type Mode = 'calibrate' | 'verify'
type Camera = 'off' | 'starting' | 'running' | 'error'
type Quality = 'searching' | 'eyes_closed' | 'tracked' | 'stale'

interface Snapshot {
  camera: Camera
  quality: Quality
  calibrated: boolean
  pointerEnabled: boolean
  pauseAvailable: boolean
  point: Point | null
  validation: { medianPoints: number; p90Points: number } | null
  message: string | null
}

interface Step {
  point: Point
  index: number
  total: number
}

const state = ref<Snapshot>({
  camera: 'off',
  quality: 'searching',
  calibrated: false,
  pointerEnabled: false,
  pauseAvailable: true,
  point: null,
  validation: null,
  message: null,
})
const connected = ref(false)
const pending = ref(false)
const sampling = ref(false)
const message = ref('')
const step = ref<Step | null>(null)
const mode = ref<Mode>('calibrate')
const exercise = ref<HTMLElement | null>(null)
let exerciseSequence = 0
let activateAfterClose = ""

const tracked = computed(() => state.value.camera === 'running' && state.value.quality === 'tracked')
const isStarting = computed(() => state.value.camera === 'starting')
const cameraActive = computed(() => state.value.camera === 'running' || isStarting.value)
const notice = computed(() => message.value || state.value.message)
const status = computed(() => {
  if (isStarting.value) return '正在打开相机'
  if (state.value.camera === 'error') return '相机需要检查'
  if (!cameraActive.value) return '相机已关闭'
  switch (state.value.quality) {
    case 'tracked': return '正在跟踪双眼'
    case 'eyes_closed': return '请睁开双眼'
    case 'stale': return '等待相机画面'
    default: return '未检测到双眼'
  }
})

function coordinates([x, y]: Point) {
  return { '--x': `${x * 100}%`, '--y': `${y * 100}%` }
}

function errorText(error: unknown) {
  if (typeof error === 'string') return error
  if (error instanceof Error) return error.message
  return '操作暂时无法完成，请重试。'
}

async function action(command: string) {
  if (pending.value) return
  pending.value = true
  message.value = ''
  try {
    await invoke(command)
  } catch (error) {
    message.value = errorText(error)
  } finally {
    pending.value = false
  }
}

async function begin(nextMode: Mode, event: MouseEvent) {
  if (pending.value) return
  pending.value = true
  message.value = ''
  activateAfterClose = (event.currentTarget as HTMLElement).id
  try {
    const firstStep = await invoke<Step>('begin_exercise', { mode: nextMode })
    mode.value = nextMode
    step.value = firstStep
    await nextTick()
    exercise.value?.focus()
  } catch (error) {
    message.value = errorText(error)
  } finally {
    pending.value = false
  }
}

async function leaveExercise() {
  step.value = null
  await nextTick()
  document.getElementById(activateAfterClose)?.focus()
  activateAfterClose = ""
}

async function capture() {
  if (!step.value || sampling.value || pending.value || !tracked.value) return
  const sequence = exerciseSequence
  sampling.value = true
  message.value = ''
  try {
    const result = await invoke<{ next: Step | null; message: string | null }>('capture_target')
    if (sequence !== exerciseSequence) return
    message.value = result.message || ''
    if (result.next) {
      step.value = result.next
    } else {
      await leaveExercise()
    }
  } catch (error) {
    if (sequence === exerciseSequence) message.value = errorText(error)
  } finally {
    if (sequence === exerciseSequence) sampling.value = false
  }
}

async function cancel() {
  if (!step.value || pending.value) return
  exerciseSequence += 1
  sampling.value = false
  pending.value = true
  try {
    await invoke('cancel_exercise')
    pending.value = false
    message.value = ''
    await leaveExercise()
  } catch (error) {
    message.value = errorText(error)
  } finally {
    pending.value = false
  }
}

function onKeydown(event: KeyboardEvent) {
  if (!step.value || event.repeat) return
  if (event.code === 'Escape') {
    event.preventDefault()
    void cancel()
  } else if (event.code === 'Space' && !(event.target instanceof HTMLButtonElement)) {
    event.preventDefault()
    void capture()
  }
}

onMounted(async () => {
  window.addEventListener('keydown', onKeydown)
  try {
    const channel = new Channel<Snapshot>((snapshot) => { state.value = snapshot })
    await invoke('subscribe', { channel })
    connected.value = true
  } catch {
    message.value = '无法连接桌面服务。请从 Eyes Mouse 应用打开此界面。'
  }
})
onUnmounted(() => window.removeEventListener('keydown', onKeydown))
</script>

<template>
  <div v-if="step" ref="exercise" class="exercise" tabindex="-1" aria-label="全屏注视目标">
    <div class="exercise-bar">
      <span>{{ mode === 'calibrate' ? '九点校准' : '五点精度验证' }} <strong>{{ step.index }} / {{ step.total }}</strong></span>
      <button type="button" class="exercise-exit" @click="cancel">退出 <kbd>Esc</kbd></button>
    </div>
    <span class="exercise-dot" :class="{ sampling }" :style="coordinates(step.point)" aria-hidden="true"></span>
    <div class="exercise-instruction" aria-live="polite">
      <strong v-if="sampling">正在采样，请保持注视</strong>
      <strong v-else-if="!tracked">请让相机看到双眼</strong>
      <strong v-else>看向亮点，按空格采样</strong>
      <p v-if="notice">{{ notice }}</p>
    </div>
  </div>

  <div v-else class="shell">
    <header class="topbar">
      <div class="wordmark">
        <span class="mark" aria-hidden="true"><i></i></span>
        <span>Eyes Mouse</span>
      </div>
      <span class="camera-status" :class="{ live: tracked }" role="status">
        <i aria-hidden="true"></i>{{ status }}
      </span>
    </header>

    <main>
      <header class="intro">
        <h1>用视线移动指针</h1>
        <p>开启相机并校准视线，在接管指针前确认定位效果。</p>
      </header>

      <div class="workspace">
        <section class="preview" aria-labelledby="preview-title">
          <header class="preview-header">
            <h2 id="preview-title">注视点预览</h2>
            <span v-if="state.pointerEnabled" class="live-label"><i aria-hidden="true"></i>指针已启用</span>
            <span v-else class="preview-mode">指针暂停</span>
          </header>
          <div class="plot" aria-hidden="true">
            <div class="plot-guides"></div>
            <span class="plot-corner top-left"></span><span class="plot-corner top-right"></span>
            <span class="plot-corner bottom-left"></span><span class="plot-corner bottom-right"></span>
            <template v-if="state.point">
              <span class="gaze-line horizontal" :style="coordinates(state.point)"></span>
              <span class="gaze-line vertical" :style="coordinates(state.point)"></span>
              <span class="gaze-point" :style="coordinates(state.point)"></span>
            </template>
            <span v-else class="idle-cross"></span>
          </div>
          <footer class="preview-footer">
            <span v-if="state.point"><i class="legend-dot" aria-hidden="true"></i>预测注视位置</span>
            <span v-else>{{ state.calibrated ? '等待有效的眼动数据' : '完成校准后显示预测位置' }}</span>
            <span v-if="state.point" class="numbers">{{ Math.round(state.point[0] * 100) }}% / {{ Math.round(state.point[1] * 100) }}%</span>
          </footer>
        </section>

        <section class="controls" aria-label="眼动控制">
          <div class="control-row">
            <div>
              <h2>摄像头</h2>
              <p>请正对主屏幕，保持脸部光线均匀。</p>
            </div>
            <button v-if="cameraActive" type="button" class="button" :disabled="pending" @click="action('stop_camera')">关闭相机</button>
            <button v-else type="button" class="button" :class="{ accent: !state.calibrated }" :disabled="!connected || pending" @click="action('start_camera')">开启相机</button>
          </div>

          <div class="control-row">
            <div>
              <h2>屏幕校准 <span v-if="state.calibrated" class="done">已完成</span></h2>
              <p>逐个注视目标并按空格。移动相机或调整坐姿后可重新校准。</p>
            </div>
            <div class="button-group">
              <button id="calibrate" type="button" class="button" :class="{ accent: tracked && !state.calibrated }" :disabled="!tracked || pending" @click="begin('calibrate', $event)">
                {{ state.calibrated ? '重新校准' : '开始九点校准' }}
              </button>
              <button v-if="state.calibrated" id="verify" type="button" class="button quiet" :disabled="!tracked || pending" @click="begin('verify', $event)">验证精度</button>
            </div>
            <p v-if="state.validation" class="results" role="status">
              五点验证误差 · 中位数 {{ state.validation.medianPoints }} pt · P90 {{ state.validation.p90Points }} pt
            </p>
          </div>

          <div class="control-row">
            <div>
              <h2>系统指针</h2>
              <p>用鼠标或触控板点击。随时按 <kbd>⌃</kbd><kbd>⌥</kbd><kbd>P</kbd> 暂停。</p>
            </div>
            <button v-if="state.pointerEnabled" type="button" class="button accent" :disabled="pending" @click="action('pause_pointer')">暂停指针</button>
            <button v-else type="button" class="button" :class="{ accent: state.calibrated && tracked }" :disabled="!state.calibrated || !tracked || !state.pauseAvailable || pending" @click="action('enable_pointer')">启用指针</button>
            <p v-if="!state.pauseAvailable" class="results">暂停快捷键不可用。释放该组合键后重启应用。</p>
          </div>
        </section>
      </div>
      <p v-if="notice" class="notice" role="status">{{ notice }}</p>
    </main>

    <footer class="app-footer">
      <span>相机画面仅在本机处理</span>
      <span>macOS · 单屏</span>
    </footer>
  </div>
</template>
