<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from 'vue'
import { Channel, invoke } from '@tauri-apps/api/core'

type Point = [number, number]
type Mode = 'calibrate' | 'verify'
type ControlMode = 'eye' | 'head'
type Camera = 'off' | 'starting' | 'running' | 'error'
type Quality = 'searching' | 'eyes_closed' | 'tracked' | 'stale'
type PointerAccess = 'missing' | 'waiting' | 'granted' | 'restart_required' | 'restarting'

interface Display {
  id: number
  name: string
  primary: boolean
}

interface Snapshot {
  camera: Camera
  quality: Quality
  controlMode: ControlMode
  displays: Display[]
  selectedDisplayId: number | null
  displayRevision: number
  eyeDamping: number
  calibrated: boolean
  pointerEnabled: boolean
  pointerAccess: PointerAccess
  pauseAvailable: boolean
  refinementAvailable: boolean
  refinementActive: boolean
  refinementRevision: number
  point: Point | null
  validation: { medianPoints: number; p90Points: number } | null
  message: string | null
  resetExercise: ResetStep | null
  snakeOpen: boolean
}

interface Step {
  point: Point
  index: number
  total: number
}

interface ResetStep extends Step {
  id: number
}

const state = ref<Omit<Snapshot, 'pointerAccess' | 'displays'> & { pointerAccess: PointerAccess | null; displays: Display[] | null }>({
  camera: 'off',
  quality: 'searching',
  controlMode: 'eye',
  displays: null,
  selectedDisplayId: null,
  displayRevision: 0,
  eyeDamping: 50,
  calibrated: false,
  pointerEnabled: false,
  pointerAccess: null,
  pauseAvailable: true,
  refinementAvailable: true,
  refinementActive: false,
  refinementRevision: 0,
  point: null,
  validation: null,
  message: null,
  resetExercise: null,
  snakeOpen: false,
})
const connected = ref(false)
const pending = ref(false)
const requestedDisplayId = ref<number | null>(null)
const sampling = ref(false)
const capturingNeutral = ref(false)
const eyeDampingDraft = ref<number | null>(null)
const savedEyeDamping = ref<number | null>(null)
const eyeDampingError = ref('')
const message = ref('')
const step = ref<Step | null>(null)
const mode = ref<Mode>('calibrate')
const exercise = ref<HTMLElement | null>(null)
const accessibilityDialog = ref<HTMLDialogElement | null>(null)
let exerciseSequence = 0
let activateAfterClose = ""
let resetFocus = ""
let savingEyeDamping = false
let restoreDisplayFocus = false

const tracked = computed(() => state.value.camera === 'running' && state.value.quality === 'tracked')
const headControl = computed(() => state.value.controlMode === 'head')
// A snapshot may arrive after the slider's newer save confirmation.
const eyeDamping = computed(() => eyeDampingDraft.value ?? savedEyeDamping.value ?? state.value.eyeDamping)
const resetStep = computed(() => state.value.resetExercise)
const activeStep = computed(() => resetStep.value ?? step.value)
const resetLocation = computed(() => ['中央', '左上', '右下'][(resetStep.value?.index ?? 1) - 1])
const isStarting = computed(() => state.value.camera === 'starting')
const cameraActive = computed(() => state.value.camera === 'running' || isStarting.value)
const authorizing = computed(() => state.value.pointerAccess === 'waiting' || state.value.pointerAccess === 'restarting')
const notice = computed(() => message.value || state.value.message)
const selectedDisplay = computed(() => state.value.displays?.find(({ id }) => id === state.value.selectedDisplayId))
const displayReady = computed(() => connected.value && Boolean(selectedDisplay.value) && requestedDisplayId.value === null)
const displayChangePending = computed(() => pending.value || requestedDisplayId.value !== null)
const displayOptions = computed(() => (state.value.displays ?? []).map((display, _, displays) => {
  const matching = displays.filter(({ name }) => name === display.name)
  const suffix = matching.length > 1 ? ` ${matching.filter(({ id }) => id < display.id).length + 1}` : ''
  return { id: display.id, label: `${display.name}${suffix}${display.primary ? '（主屏幕）' : ''}` }
}))
const displayPlaceholder = computed(() => {
  if (!state.value.displays) return message.value ? '屏幕列表不可用' : '正在读取屏幕…'
  return state.value.displays.length ? '请选择屏幕' : '未检测到屏幕'
})
const displayHint = computed(() => {
  if (!state.value.displays) return ''
  if (requestedDisplayId.value !== null) return '正在切换屏幕…'
  if (!state.value.displays.length) return '连接屏幕后即可校准和移动指针。'
  if (!selectedDisplay.value) return '选择屏幕后即可校准和移动指针。'
  if (state.value.calibrated) {
    return headControl.value ? '切换屏幕后，指针会暂停，需要重新设定正面位置。' : '切换屏幕后，指针会暂停，需要重新校准。'
  }
  return '指针只在所选屏幕上移动。'
})
const status = computed(() => {
  if (isStarting.value) return '正在打开相机'
  if (state.value.camera === 'error') return '相机需要检查'
  if (!cameraActive.value) return '相机已关闭'
  switch (state.value.quality) {
    case 'tracked': return headControl.value ? '正在跟踪头部' : '正在跟踪双眼'
    case 'eyes_closed': return '请睁开双眼'
    case 'stale': return '等待相机画面'
    default: return headControl.value ? '未检测到脸部' : '未检测到双眼'
  }
})
const headPreview = computed(() => {
  if (capturingNeutral.value) return '正在采样，请保持正面'
  const point = state.value.point
  if (!point) return state.value.calibrated ? '等待有效的头动数据' : '设定正面位置后显示移动方向'
  const [x, y] = point
  const direction = (x < 0.5 ? '左' : x > 0.5 ? '右' : '') + (y < 0.5 ? '上' : y > 0.5 ? '下' : '')
  return direction ? `向${direction}移动` : '已回正，指针静止'
})

function coordinates([x, y]: Point) {
  return { '--x': `${x * 100}%`, '--y': `${y * 100}%` }
}

function errorText(error: unknown) {
  if (typeof error === 'string') return error
  if (error instanceof Error) return error.message
  return '操作暂时无法完成，请重试。'
}

async function action(command: string, args?: Record<string, unknown>) {
  if (pending.value) return false
  pending.value = true
  message.value = ''
  try {
    await invoke(command, args)
    return true
  } catch (error) {
    message.value = errorText(error)
    return false
  } finally {
    pending.value = false
  }
}

async function changeControlMode(event: Event) {
  const select = event.currentTarget as HTMLSelectElement
  const controlMode = select.value as ControlMode
  if (!await action('set_control_mode', { controlMode })) select.value = state.value.controlMode
}

async function changeTargetDisplay(event: Event) {
  const select = event.currentTarget as HTMLSelectElement
  if (displayChangePending.value || !select.value) return
  const displayId = Number(select.value)
  if (displayId === selectedDisplay.value?.id) return
  restoreDisplayFocus = select === document.activeElement
  requestedDisplayId.value = displayId
  if (!await action('set_target_display', { displayId })) {
    requestedDisplayId.value = null
    select.value = String(selectedDisplay.value?.id ?? '')
  }
}

// Channel snapshots can arrive before or after the selection command resolves.
watch(() => state.value.selectedDisplayId, (id) => {
  if (id === requestedDisplayId.value) requestedDisplayId.value = null
}, { flush: 'sync' })

watch(displayChangePending, (changing) => {
  if (changing || !restoreDisplayFocus) return
  restoreDisplayFocus = false
  if (document.activeElement === document.body) document.getElementById('target-display')?.focus()
}, { flush: 'post' })

async function changeEyeDamping(event: Event) {
  eyeDampingDraft.value = (event.currentTarget as HTMLInputElement).valueAsNumber
  eyeDampingError.value = ''
  if (savingEyeDamping) return
  savingEyeDamping = true
  // Keep one native write in flight and apply the latest input as soon as it finishes.
  while (eyeDampingDraft.value !== null) {
    const requested: number = eyeDampingDraft.value
    try {
      savedEyeDamping.value = await invoke<number>('set_eye_damping', { eyeDamping: requested })
    } catch (error) {
      if (eyeDampingDraft.value === requested) eyeDampingError.value = errorText(error)
    }
    if (eyeDampingDraft.value === requested) eyeDampingDraft.value = null
  }
  savingEyeDamping = false
}

async function captureNeutral() {
  if (pending.value || !displayReady.value) return
  capturingNeutral.value = true
  try {
    await action('capture_neutral')
  } finally {
    capturingNeutral.value = false
  }
}

async function begin(nextMode: Mode, event: MouseEvent) {
  if (pending.value || !displayReady.value) return
  const revision = state.value.displayRevision
  pending.value = true
  message.value = ''
  activateAfterClose = (event.currentTarget as HTMLElement).id
  try {
    const firstStep = await invoke<Step>('begin_exercise', { mode: nextMode })
    if (state.value.displayRevision !== revision) {
      await invoke('cancel_exercise')
      return
    }
    mode.value = nextMode
    step.value = firstStep
    await nextTick()
    exercise.value?.focus()
  } catch (error) {
    message.value = errorText(error)
  } finally {
    pending.value = false
    if (!activeStep.value && activateAfterClose) await leaveExercise()
  }
}

function restoreExerciseFocus(id: string) {
  const target = document.getElementById(id)
  const display = document.getElementById('target-display')
  if (target && !target.matches(':disabled')) target.focus()
  else if (display && !display.matches(':disabled')) display.focus()
  else document.getElementById('camera-toggle')?.focus()
}

async function leaveExercise() {
  step.value = null
  await nextTick()
  restoreExerciseFocus(activateAfterClose)
  activateAfterClose = ""
}

watch(() => state.value.displayRevision, () => {
  if (!step.value) return
  exerciseSequence += 1
  sampling.value = false
  message.value = ''
  void leaveExercise()
})

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
  if (!activeStep.value || pending.value) return
  const resetting = Boolean(resetStep.value)
  exerciseSequence += 1
  sampling.value = false
  pending.value = true
  try {
    await invoke('cancel_exercise')
    pending.value = false
    message.value = ''
    if (!resetting) await leaveExercise()
  } catch (error) {
    message.value = errorText(error)
  } finally {
    pending.value = false
  }
}

function onKeydown(event: KeyboardEvent) {
  if (!activeStep.value || event.repeat) return
  if (event.code === 'Escape') {
    event.preventDefault()
    void cancel()
  } else if (step.value && event.code === 'Space' && !(event.target instanceof HTMLButtonElement)) {
    event.preventDefault()
    void capture()
  }
}

function cancelPointerAccess() {
  if (state.value.pointerAccess === 'waiting') void action('cancel_pointer_access')
}

watch(authorizing, (open) => {
  const dialog = accessibilityDialog.value
  if (!dialog) return
  if (open && !dialog.open) dialog.showModal()
  if (!open && dialog.open) dialog.close()
}, { flush: 'post' })

watch(() => resetStep.value && `${resetStep.value.id}:${resetStep.value.index}`, async (key, previous) => {
  if (!key) {
    await nextTick()
    if (!resetStep.value) restoreExerciseFocus(resetFocus)
    resetFocus = ''
    return
  }
  const reset = resetStep.value
  if (!reset) return
  if (reset.id !== Number(previous?.split(':')[0])) {
    resetFocus = document.activeElement?.id || 'calibrate'
    message.value = ''
    await nextTick()
    exercise.value?.focus()
  }
  // Acknowledge the target after the browser has painted the new position.
  await new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())))
  if (resetStep.value?.id !== reset.id || resetStep.value.index !== reset.index) return
  try {
    await invoke('reset_target_ready', { id: reset.id, index: reset.index })
  } catch (error) {
    message.value = errorText(error)
  }
})

onMounted(async () => {
  window.addEventListener('keydown', onKeydown)
  try {
    const channel = new Channel<Snapshot>((snapshot) => {
      if (snapshot.refinementRevision !== state.value.refinementRevision) message.value = ''
      state.value = snapshot
    })
    await invoke('subscribe', { channel })
    connected.value = true
  } catch {
    message.value = '无法连接桌面服务。请从 IngridMeow 应用打开此界面。'
  }
})
onUnmounted(() => window.removeEventListener('keydown', onKeydown))
</script>

<template>
  <div v-if="activeStep" ref="exercise" class="exercise" tabindex="-1" aria-label="全屏注视目标">
    <div class="exercise-bar">
      <span>{{ resetStep ? '快速回正' : mode === 'calibrate' ? '九点校准' : '五点精度验证' }} <strong>{{ activeStep.index }} / {{ activeStep.total }}</strong></span>
      <button type="button" class="exercise-exit" @click="cancel">退出 <kbd>Esc</kbd></button>
    </div>
    <span class="exercise-dot" :class="{ sampling }" :style="coordinates(activeStep.point)" aria-hidden="true"></span>
    <div class="exercise-instruction" aria-live="polite">
      <strong v-if="sampling">正在采样，请保持视线和头部稳定</strong>
      <strong v-else-if="!tracked">请让相机看到双眼</strong>
      <strong v-else-if="resetStep">注视{{ resetLocation }}的亮点，保持头部稳定</strong>
      <strong v-else>看准亮点后按空格采样</strong>
      <p v-if="notice">{{ notice }}</p>
    </div>
  </div>

  <div v-else class="shell">
    <header class="topbar">
      <div class="wordmark">
        <span class="mark" aria-hidden="true"><i></i></span>
        <span>IngridMeow</span>
      </div>
      <div class="topbar-actions">
        <span class="camera-status" :class="{ live: tracked }" role="status">
          <i aria-hidden="true"></i>{{ status }}
        </span>
        <template v-if="state.snakeOpen">
          <button type="button" class="button" :disabled="pending" @click="action('restart_snake')">重新开始</button>
          <button type="button" class="button" :disabled="pending" @click="action('close_snake')">关闭游戏</button>
        </template>
        <button v-else type="button" class="button" :disabled="!connected || pending" @click="action('open_snake')">贪吃喵</button>
      </div>
    </header>

    <main>
      <header class="intro">
        <h1>{{ headControl ? '用头部动作移动指针' : '用视线移动指针' }}</h1>
        <p>{{ headControl ? '开启相机并设定正面位置，先预览头部动作对应的移动方向。' : '开启相机并校准视线，在接管指针前确认定位效果。' }}</p>
      </header>

      <div class="workspace">
        <section class="preview" aria-labelledby="preview-title">
          <header class="preview-header">
            <h2 id="preview-title">{{ headControl ? '头动方向预览' : '注视点预览' }}</h2>
            <span v-if="state.pointerEnabled" class="live-label"><i aria-hidden="true"></i>指针已启用</span>
            <span v-else-if="state.refinementActive" class="preview-mode">正在局部校正</span>
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
            <span v-if="headControl"><i v-if="state.point" class="legend-dot" aria-hidden="true"></i>{{ headPreview }}</span>
            <template v-else>
              <span v-if="state.point"><i class="legend-dot" aria-hidden="true"></i>预测注视位置</span>
              <span v-else>{{ state.calibrated ? '等待有效的眼动数据' : '完成校准后显示预测位置' }}</span>
              <span v-if="state.point" class="numbers">{{ Math.round(state.point[0] * 100) }}% / {{ Math.round(state.point[1] * 100) }}%</span>
            </template>
          </footer>
        </section>

        <section class="controls" aria-label="指针控制设置">
          <div class="control-row">
            <h2><label for="control-mode">控制动作</label></h2>
            <select id="control-mode" class="button control-select" :value="state.controlMode" :disabled="!connected || pending" @change="changeControlMode">
              <option value="eye">眼动（注视）</option>
              <option value="head">头动（转头与点头）</option>
            </select>
          </div>

          <div class="control-row display-control">
            <h2><label for="target-display">控制屏幕</label></h2>
            <select id="target-display" class="button control-select" :value="requestedDisplayId ?? selectedDisplay?.id ?? ''"
              :disabled="!connected || displayChangePending || authorizing || !displayOptions.length"
              :title="selectedDisplay?.name"
              aria-describedby="target-display-hint" @change="changeTargetDisplay">
              <option value="" disabled>{{ displayPlaceholder }}</option>
              <option v-for="display in displayOptions" :key="display.id" :value="display.id">{{ display.label }}</option>
            </select>
            <p v-show="displayHint" id="target-display-hint" role="status">{{ displayHint }}</p>
          </div>

          <div class="control-row">
            <div>
              <h2>
                辅助功能权限
                <span v-if="state.pointerAccess === 'granted' || state.pointerAccess === 'restart_required'" class="done">已有权限</span>
              </h2>
              <p v-if="state.pointerAccess === 'missing'">请先授权 IngridMeow 控制系统指针。</p>
              <p v-else-if="state.pointerAccess === 'restart_required'">请按 ⌘Q 退出，再打开 IngridMeow。</p>
            </div>
            <button v-if="state.pointerAccess === 'missing'" type="button" class="button accent" :disabled="!connected || pending" @click="action('request_pointer_access')">授权辅助功能</button>
          </div>

          <div class="control-row">
            <div>
              <h2>摄像头</h2>
              <p>调整相机和光线，让脸部清晰可见。</p>
            </div>
            <button v-if="cameraActive" id="camera-toggle" type="button" class="button" :disabled="pending" @click="action('stop_camera')">关闭相机</button>
            <button v-else id="camera-toggle" type="button" class="button" :class="{ accent: state.pointerAccess === 'granted' && !state.calibrated }" :disabled="!connected || pending" @click="action('start_camera')">开启相机</button>
          </div>

          <div v-if="headControl" class="control-row">
            <div>
              <h2>正面位置 <span v-if="state.calibrated" class="done">已设定</span></h2>
              <p>面向屏幕，保持稳定直到采样完成。转头、抬头或低头控制方向，回正即停。</p>
            </div>
            <button type="button" class="button" :class="{ accent: tracked && !state.calibrated }" :disabled="!displayReady || !tracked || pending" @click="captureNeutral">
              {{ capturingNeutral ? '正在采样…' : state.calibrated ? '重新设定正面位置' : '设定正面位置' }}
            </button>
          </div>

          <div v-else class="control-row">
            <div>
              <h2>屏幕校准 <span v-if="state.calibrated" class="done">已完成</span></h2>
              <p>看准每个亮点后按空格，保持注视，等亮点切换。移动相机或调整坐姿后可重新校准。</p>
            </div>
            <div class="button-group">
              <button id="calibrate" type="button" class="button" :class="{ accent: tracked && !state.calibrated }" :disabled="!displayReady || !tracked || pending" @click="begin('calibrate', $event)">
                {{ state.calibrated ? '重新校准' : '开始九点校准' }}
              </button>
              <button v-if="state.calibrated" id="verify" type="button" class="button quiet" :disabled="!displayReady || !tracked || pending" @click="begin('verify', $event)">验证精度</button>
            </div>
            <p v-if="state.validation" class="results" role="status">
              五点验证误差 · 中位数 {{ state.validation.medianPoints }} pt · P90 {{ state.validation.p90Points }} pt
            </p>
          </div>

          <div v-if="!headControl" class="control-row damping-control">
            <div class="damping-heading">
              <h2><label for="eye-damping">眼动阻尼</label></h2>
              <span class="numbers" aria-hidden="true">{{ eyeDamping }}</span>
            </div>
            <input id="eye-damping" class="damping-slider" type="range" min="0" max="100" step="1"
              :value="eyeDamping" :disabled="!connected || pending" aria-describedby="eye-damping-hint" @input="changeEyeDamping">
            <p id="eye-damping-hint">数值越低，跟随越快；数值越高，移动越稳。</p>
            <p v-if="eyeDampingError" class="damping-error" role="status">{{ eyeDampingError }}</p>
          </div>

          <div class="control-row">
            <div>
              <h2>系统指针</h2>
              <p>用鼠标或触控板点击。随时按 <kbd>⌃</kbd><kbd>⌥</kbd><kbd>P</kbd> 暂停。</p>
              <template v-if="!headControl">
                <p>校准后头部保持稳定，眼睛依次看左 → 右 → 左 → 右，每边稍停，即可启用或暂停指针。</p>
                <p>用视线逆时针画一圈可退出应用。</p>
                <p>完成九点校准后，可快速眨眼三次回正。依次注视中央、左上和右下的亮点。</p>
                <p v-if="state.calibrated && state.refinementAvailable">看准目标约一秒后按 <kbd>⌃</kbd><kbd>⇧</kbd><kbd>M</kbd>，用鼠标移到目标，再按一次保存局部校正。</p>
                <p v-else-if="state.calibrated && state.pauseAvailable">局部校正快捷键 <kbd>⌃</kbd><kbd>⇧</kbd><kbd>M</kbd> 不可用。释放该组合键后重启应用。</p>
              </template>
            </div>
            <button v-if="state.pointerEnabled || state.refinementActive" type="button" class="button accent" :disabled="pending" @click="action('pause_pointer')">{{ state.refinementActive ? '取消局部校正' : '暂停指针' }}</button>
            <button v-else type="button" class="button" :class="{ accent: state.calibrated && tracked }" :disabled="!displayReady || !state.calibrated || !tracked || !state.pauseAvailable || pending" @click="action('enable_pointer')">启用指针</button>
            <p v-if="!state.pauseAvailable" class="results">暂停快捷键 <kbd>⌃</kbd><kbd>⌥</kbd><kbd>P</kbd> 不可用。释放该组合键后重启应用。</p>
          </div>
        </section>
      </div>
      <p v-if="notice && !authorizing" class="notice" role="status">{{ notice }}</p>
    </main>

    <footer class="app-footer">
      <span>相机画面仅在本机处理</span>
      <span>macOS</span>
    </footer>
  </div>

  <dialog ref="accessibilityDialog" class="accessibility-dialog" aria-labelledby="accessibility-title" @cancel.prevent="cancelPointerAccess">
    <h2 id="accessibility-title">授权辅助功能</h2>
    <p v-if="state.pointerAccess === 'restarting'" role="status">权限已开启，正在重新启动…</p>
    <template v-else>
      <p>在「系统设置 → 隐私与安全性 → 辅助功能」中开启 IngridMeow。授权后，应用会自动重新启动。</p>
      <p v-if="state.calibrated">{{ headControl ? '重启后需要重新设定正面位置。' : '重启后需要重新校准视线。' }}</p>
      <p v-if="message" class="notice" role="alert">{{ message }}</p>
      <div class="button-group dialog-actions">
        <button type="button" class="button" :disabled="pending" @click="cancelPointerAccess">取消</button>
        <button type="button" class="button accent" :disabled="pending" autofocus @click="action('open_accessibility_settings')">打开系统设置</button>
      </div>
    </template>
  </dialog>
</template>
