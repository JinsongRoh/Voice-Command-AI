/* Voice Command AI landing page: language switch (EN | KO | JA | ES | ZH | RU) and copy button.
   English lives in the HTML; Korean, Japanese, Spanish, Simplified Chinese and Russian strings live here. No tracking, no cookies.
   localStorage only remembers the language choice. */
(function () {
  "use strict";

  var STORAGE_KEY = "vca-lang";
  var LANGS = ["en", "ko", "ja", "es", "zh", "ru"];
  // The page language code; Chinese is Simplified (zh-CN) so fonts and screen readers pick the right variant.
  var HTML_LANG = { zh: "zh-CN" };
  var REPORT = "https://github.com/JinsongRoh/Voice-Command-AI/blob/main/docs/history/m0-report.md";
  var TTS = "https://github.com/JinsongRoh/Voice-Command-AI/blob/main/docs/tts-benchmark.md";
  var DNSPILOT = "https://dnspilot.kr";

  var KO = {
    "meta.title": "Voice Command AI · 말로 지휘하고, 흐름을 관리하다.",
    "meta.desc": "내 PC의 AI 코딩 에이전트를 휴대폰에서 음성으로 확인하고 지시하는 오픈소스 도구입니다. Claude Code, Codex, Gemini CLI 등을 지원하며 종단 간 암호화와 MIT 라이선스를 적용했습니다.",
    "meta.ogTitle": "Voice Command AI: 말로 지휘하고, 흐름을 관리하다.",
    "meta.ogDesc": "내 PC의 모든 AI 코딩 에이전트를 휴대폰에서 음성으로 지휘하세요. 오픈소스, MIT 라이선스.",

    "skip": "본문으로 건너뛰기",
    "brand.home": "Voice Command AI 홈",
    "nav.label": "주요 메뉴",
    "nav.why": "왜 VCA인가",
    "nav.features": "기능",
    "nav.how": "작동 방식",
    "nav.measured": "검증 결과",
    "nav.oss": "오픈소스",
    "lang.label": "언어",
    "gh.aria": "GitHub의 Voice Command AI",

    "hero.symbolAlt": "Voice Command AI 심볼",
    "hero.badge": "버전 0.1.1 · 이제 Linux도 지원",
    "hero.title": "<span class=\"line\">말로 지휘하고,</span> <span class=\"line\">흐름을 관리하다.</span>",
    "hero.lead": "내 PC의 모든 AI 코딩 에이전트를, 휴대폰에서 목소리로 지휘하세요.",
    "hero.sub": "PC에서 도는 가벼운 서버가 모든 에이전트 세션을 맡습니다. 휴대폰에는 세션이 카드로 보이고, 명령은 창 포커스를 빼앗지 않고 정확한 세션에 도착합니다.",
    "cta.github": "GitHub에서 보기",
    "cta.guide": "사용자 가이드 (영문)",
    "hero.meta1": "MIT 라이선스",
    "hero.meta2": "종단 간 암호화",
    "hero.meta3": "내 PC에서 실행, VCA 클라우드 없음",

    "g.alt": "VCA 앱 예시 화면: 세션 카드 세 장과 api-server 세션으로 보낼 음성 명령",
    "g.conn": "연결됨 · 암호화",
    "g.run": "실행 중",
    "g.wait": "입력 필요",
    "g.done": "완료",
    "g.msg1": "테스트 스위트 실행 중…",
    "g.msg2": "데이터베이스 마이그레이션을 적용할까요?",
    "g.msg3": "테스트 완료. 실패한 2개 항목을 확인하세요.",
    "g.to": "받는 세션",
    "g.text": "통합 테스트 돌리고 실패한 항목만 요약해 줘.",
    "g.send": "보내기",
    "g.res": "통합 테스트 완료. 실패 2건을 요약했습니다.",
    "g.caption": "예시 화면입니다. 보내기 전에 어느 세션으로 갈지 먼저 보여 줍니다.",

    "why.eyebrow": "왜 VCA인가",
    "why.title": "에이전트는 여럿, 키보드는 하나.",
    "why.intro": "에이전트 세션을 여러 개 띄우면 서브 에이전트, 컴파일러, 테스트 러너, 보조 창이 한 대의 PC로 몰립니다. 가장 먼저 무너지는 건 이 세 가지입니다.",
    "why.with": "VCA는 이렇게",
    "why.p1.title": "포커스 탈취",
    "why.p1.body": "새 창이 계속 포커스를 가져가서 프롬프트와 키 입력이 엉뚱한 터미널로 들어갑니다.",
    "why.p1.fix": "가상 터미널을 통해 대상 세션에 명령을 직접 전달합니다. 키 입력을 흉내 내지 않습니다.",
    "why.p2.title": "자원 고갈",
    "why.p2.body": "에이전트들이 서로를 모른 채 빌드와 테스트를 시작하고, 결국 CPU와 메모리가 바닥나 PC가 멈춥니다.",
    "why.p2.fix": "리소스 브로커가 무거운 작업을 정해진 슬롯 수만큼만 실행하고 나머지는 대기열에 둡니다.",
    "why.p3.title": "탐색 부담",
    "why.p3.body": "기존 원격 제어 앱은 명령 하나를 보낼 때마다 프로젝트에 들어갔다 나오기를 반복하게 합니다.",
    "why.p3.fix": "모든 세션이 카드 한 벌에 모입니다. 고르고, 말하고, 보내면 끝입니다.",

    "feat.eyebrow": "기능",
    "feat.title": "지금 VCA로 할 수 있는 일",
    "feat.intro": "‘출시 예정’ 표시가 있는 항목을 빼면 아래 기능은 모두 버전 0.1.1에 들어 있습니다.",
    "soon": "출시 예정",
    "f1.t": "세션 카드",
    "f1.d": "PC의 모든 에이전트 세션을 실시간 카드로 보여 줍니다. 프로젝트, 에이전트, 상태, 알림을 한눈에 확인하세요. 이미 열려 있는 AI CLI 세션을 가져오거나, 고른 폴더에서 새 세션을 시작할 수 있습니다. 카드마다 그 세션이 돌리는 서브 에이전트 수도 보여 줍니다.",
    "f2.t": "음성 우선",
    "f2.d": "탭하고 말하면 받아쓰기가 말하는 대로 실시간으로 나타납니다. 내용을 확인하고 보내세요. 개발자 용어 엔진이 기술 용어를 정확하게 지켜 줍니다.",
    "f3.t": "포커스가 필요 없음",
    "f3.d": "명령은 가상 터미널이나 에이전트 고유 채널로 대상 세션에 바로 전달됩니다. 키 입력 시뮬레이션은 쓰지 않습니다.",
    "f4.t": "실시간 보기",
    "f4.d": "어떤 세션이든 읽기 쉬운 채팅 화면이나 원본 터미널로 따라가며 대화를 이어 가세요. 채팅 검색, 마크다운 표, 페이지 단위로 불러오는 이전 기록도 지원합니다.",
    "f5.t": "알림, 원하면 정확하게",
    "f5.d": "별도의 알림 메뉴가 결정이나 입력을 기다리는 세션을 모아 보여 주고, 개수를 배지로 표시합니다. 설정에서 직접 설치하는 선택형 에이전트 훅을 쓰면 승인 알림이 정확해집니다. 탭 한 번으로 답하세요.",
    "f6.t": "내 음성 엔진으로 듣는 알림",
    "f6.d": "직접 운영하는 OpenAI 호환 TTS 엔진으로 알림을 들으세요. MeloTTS와 Supertonic용 레퍼런스 서버도 있습니다. PC 음성을 고르면 모든 음성 안내가 그 목소리로 한 번에 하나씩 나오고, 버튼 하나로 켜고 끕니다. 기기 음성은 PC 음성이 시작되기 전에만 대신합니다.",
    "f7.t": "에이전트가 요청한 할 일",
    "f7.d": "로그인, 승인, 명령 실행처럼 에이전트가 나에게 부탁하는 일이 한 목록에 모입니다. 체크해서 끝내고 결과를 세션으로 돌려보내세요. 명령을 대신 실행하는 일은 없습니다. 관리자 권한이 필요한 작업은 운영체제의 권한 확인 창을 거쳐서만 관리자 셸을 엽니다.",
    "f8.t": "재부팅 후 창 복원",
    "f8.d": "재부팅이나 정전 뒤에도 AI CLI 창을 같은 자리, 같은 대화로 다시 엽니다. Windows, macOS, Linux, tmux를 지원합니다.",
    "f9.t": "단단히 잠근 원격 셸",
    "f9.d": "에이전트가 막혔을 때 휴대폰에서 PC의 셸을 엽니다. 관리자 권한으로 페어링한 기기만 열 수 있고, 셸마다 Face ID 같은 생체 인증과 새 인증 코드가 필요하며, 정해진 시간이 지나면 저절로 닫힙니다.",
    "f10.t": "휴대폰에 맞춘 화면",
    "f10.d": "세션을 캐러셀로 넘겨 보고, 사진, 파일, 간단한 스케치를 첨부하고, 라이트, 다크, 완전한 블랙 테마 중에서 고르세요. PC가 아직 없다면 샘플 세션으로 데모를 체험해 보세요(오프라인). 한국어, 영어, 일본어, 스페인어를 지원합니다.",
    "f11.t": "리소스 브로커",
    "f11.d": "세션별 CPU, 메모리, 디스크, 네트워크를 지켜봅니다. 무거운 작업은 정책에 따라 실행, 대기, 제한, 일시 정지됩니다.",
    "f12.t": "페어링과 암호화",
    "f12.d": "일회용 페어링 링크, TOTP 2단계 인증, 단기 토큰, Noise 종단 간 암호화, 감사 로그, 킬 스위치를 갖췄습니다.",
    "f13.t": "내 PC, 내 연결 방식",
    "f13.d": "집, 사무실, 서버 PC를 앱 하나에 페어링하세요. LAN 직접 연결, Tailscale, Cloudflare Tunnel, DNSPilot, 또는 직접 구성한 방식으로 접속합니다. 연결 방식을 정하기 전까지 서버는 로컬호스트에서만 대기합니다.",
    "f14.t": "엔진은 직접 선택",
    "f14.d": "음성 인식(예: Whisper 서버), 교정용 LLM, 음성 합성을 로컬이나 내 클라우드 계정으로 직접 고릅니다. VCA는 음성 서버도, 모델 가중치도 포함하지 않습니다.",
    "f15.t": "앱이 닫혀 있을 때 받는 휴대폰 알림",
    "f15.d": "이번 릴리스에는 포함되지 않았으며 추후 제공할 예정입니다. 지금은 휴대폰에서는 VCA 앱이 열려 있을 때 알림을 받고, 데스크톱에서는 백그라운드에서도 시스템 알림이 뜹니다.",

    "c.eyebrow": "지휘소",
    "c.title": "모든 에이전트를 지켜보는 지휘 세션.",
    "c.intro": "선택형 지휘 세션을 켜고 “나를 기다리는 세션 있어?”, “api 세션에 테스트 돌리라고 해 줘”처럼 물어보세요. VCA가 보는 정보로 답하고 VCA의 MCP 도구로 움직이지만, 실행은 내가 허용한 다음에만 합니다.",
    "c.l1": "다른 세션으로 보내는 모든 메시지는 내가 허용하거나 거부할 때까지 기다립니다.",
    "c.l2": "권한 승인 요청에는 절대 답하지 않습니다. 그 결정은 늘 내 몫입니다.",
    "c.l3": "작업 횟수에는 시간당 상한이 있고, 기본값은 20회입니다.",
    "c.l4": "긴급 정지로 멈추면 대기 중인 요청도 모두 거부됩니다.",
    "c.l5": "모든 작업이 감사 로그에 남습니다.",
    "c.runs": "사용할 수 있는 에이전트",
    "c.builtin": "내장 (내 LLM 사용)",
    "c.note": "기본값은 꺼짐이며, PC의 데스크톱 앱에서만 켤 수 있습니다.",
    "c.link": "지휘 세션 자세히 보기 (영문)",
    "c.alt": "지휘소 화면 예시: 지휘 세션이 api-server 세션에 메시지를 보내도 되는지 묻고, 허용 또는 거부를 기다립니다.",
    "c.p.title": "지휘소",
    "c.p.state": "응답 대기 중",
    "c.p.you": "api-server에 통합 테스트 돌리라고 전해 줘.",
    "c.p.req": "지휘 세션이 <strong>api-server</strong> 세션에 다음을 보내려 합니다:",
    "c.p.msg": "통합 테스트를 실행하고 실패한 항목을 요약해 줘.",
    "c.p.allow": "허용",
    "c.p.deny": "거부",
    "c.p.timer": "2분 안에 답하지 않으면 자동으로 거부됩니다",
    "c.p.cap": "이번 시간 작업",
    "c.p.kill": "긴급 정지",

    "how.eyebrow": "작동 방식",
    "how.title": "휴대폰은 오직 내 PC와만 통신합니다.",
    "how.intro": "한 번 페어링하면 앱은 Noise 종단 간 암호화 채널로 PC의 VCA 서버에 연결됩니다. 에이전트 세션은 서버가 직접 관리하며, 어떤 데이터도 VCA 클라우드를 거치지 않습니다.",
    "d.alt": "구성도: 휴대폰이나 데스크톱의 VCA 앱이 종단 간 암호화 채널로 내 PC의 VCA 서버에 연결되고, 서버는 Claude Code, Codex, Gemini CLI, Antigravity, Grok 같은 에이전트를 관리하며 선택적으로 내 음성 인식, LLM, 음성 합성 엔진을 사용합니다.",
    "d.app": "VCA 앱",
    "d.appSub": "휴대폰 · 데스크톱",
    "d.app1": "세션 카드",
    "d.app2": "음성 받아쓰기",
    "d.app3": "실시간 보기와 알림",
    "d.e2e": "종단 간 암호화",
    "d.noise": "Noise 프로토콜",
    "d.via1": "LAN · Tailscale · Cloudflare",
    "d.via2": "DNSPilot · 사용자 지정",
    "d.server": "VCA 서버",
    "d.serverSub": "내 PC에서 실행",
    "d.s1": "세션 관리자 (PTY)",
    "d.s2": "에이전트 어댑터",
    "d.s3": "리소스 브로커",
    "d.s4": "음성 파이프라인",
    "d.s5": "인증 · 감사 로그 · API",
    "d.agents": "내 PC의 에이전트",
    "d.more": "그 외",
    "d.stt": "내 음성 엔진 (선택)",
    "d.sttSub": "STT · LLM · TTS, OpenAI 호환",
    "d.caption": "파란 테두리는 전용 어댑터가 있는 에이전트입니다. 나머지는 범용 어댑터로 동작합니다.",
    "s1.t": "VCA로 에이전트 실행",
    "s1.d": "관리 세션은 창 포커스와 상관없이 앱에서 입력을 받습니다.",
    "s2.t": "한 번만 페어링",
    "s2.d": "데스크톱 앱에서 설정 › 연결·보안 › 휴대폰 연결을 열고, VCA 앱으로 QR 코드를 스캔하세요. 한 번 스캔하면 페어링되고, DNSPilot을 쓰면 원격 접속 등록까지 함께 끝납니다. 터미널에서는:",
    "s3.t": "카드를 고르고 말하기",
    "s3.d": "세션을 고르고 말한 뒤, 대상과 내용을 확인하고 보냅니다.",

    "m.eyebrow": "초기 측정 결과",
    "m.title": "실제 장비에서 측정한 숫자입니다.",
    "m.intro": "M0 마일스톤에서는 2026년 10월, Windows, macOS, Linux, iPhone에서 핵심 구성 요소를 하나씩 검증했습니다. 주요 결과는 다음과 같습니다.",
    "m.s1": "LAN과 Tailscale에서 각각 2시간 연속 테스트한 요청 응답 수. 유실 0건",
    "m.v2": "1.3<small>초</small>",
    "m.s2": "서버 재시작 후 다시 연결되기까지 걸린 시간. 재페어링 필요 없음",
    "m.v3": "~0.3<small>초</small>",
    "m.s3": "약 10초 분량의 음성을 GPU Whisper 서버에서 받아쓰는 데 걸린 시간",
    "m.v4": "5.5<small>ms</small>",
    "m.s4": "암호화 채널을 거친 LAN 왕복 지연 시간의 중앙값",
    "m.c1": "Windows·macOS에서 관리 세션 검증",
    "m.c2": "실제 Claude Code 훅과 Codex 입력 검증",
    "m.c3": "실제 cargo 빌드로 실행 승인 제어 검증",
    "m.c4": "iPhone 음성 입력부터 받아쓰기까지 검증",
    "m.note": "0.1.0 출시 전인 2026년 10월, 첫 테스트 빌드에서 측정한 결과입니다. <a href=\"" + REPORT + "\">전체 테스트 보고서 읽기 (영문)</a>",
    "m.tts": "음성 합성 엔진은 따로 비교했습니다. 오픈소스 엔진 7종에 같은 문장을 한국어, 영어, 일본어, 스페인어로 읽게 했습니다. <a href=\"" + TTS + "\">TTS 벤치마크 읽기 (영문)</a>",

    "p.eyebrow": "지원 범위",
    "p.title": "쓰던 에이전트를 그대로 쓰세요.",
    "p.agentsTitle": "에이전트",
    "p.agentsBody": "터미널에서 도는 에이전트라면 관리 세션으로 실행할 수 있습니다. Claude Code와 Codex는 전용 어댑터를 지원하고, 나머지는 출력에서 프롬프트와 대기 상태를 읽어 내는 범용 어댑터로 동작합니다.",
    "p.more": "그 외",
    "p.legend": "전용 어댑터",
    "p.tm": "제품명은 각 소유자의 상표입니다.",
    "p.platTitle": "플랫폼",
    "p.platBody": "서버는 개발 PC에서, 앱은 데스크톱과 휴대폰에서 실행됩니다. 데스크톱 설치 파일은 GitHub Releases에서, Android 앱은 Google Play에서 받을 수 있고, iPhone 앱은 App Store에서 심사 중입니다.",
    "p.kindDesk": "서버와 데스크톱 앱",
    "p.kindMac": "서버와 데스크톱 앱 · 서명 및 공증 완료",
    "p.kindApp": "모바일 앱",
    "p.src": "소스 빌드",
    "p.internal": "내부 테스트 빌드",

    "t.eyebrow": "직접 써 보기",
    "t.title": "소스에서 직접 빌드할 수도 있습니다.",
    "t.body": "소스에서 서버와 CLI를 설치하고, 서버를 띄운 다음, 첫 에이전트를 VCA로 실행하세요.",
    "t.reqs": "Git과 Rust 툴체인이 필요합니다. Windows에서는 ‘C++를 사용한 데스크톱 개발’ 빌드 도구도 설치하세요.",
    "t.copy": "복사",
    "t.copied": "복사했습니다",
    "t.codeLabel": "빠른 시작 명령어",

    "o.eyebrow": "오픈소스",
    "o.title": "MIT 라이선스. 모든 기능을 무료로.",
    "o.intro": "코어는 Rust로, 앱은 Tauri 2로 만들었습니다. 어떤 기능도 비용을 내지 않고 쓸 수 있습니다.",
    "o.c1.t": "코드 살펴보기",
    "o.c1.d": "서버, CLI, 공용 UI, 데스크톱·모바일 앱이 MIT 라이선스로 한 저장소에 있습니다.",
    "o.c1.l": "저장소 둘러보기",
    "o.c2.t": "기여하기",
    "o.c2.d": "코드, 에이전트 어댑터, 번역, 내 언어의 개발자 용어 사전까지. 이슈와 풀 리퀘스트를 환영합니다.",
    "o.c2.l": "이슈 열기",
    "o.c3.t": "프로젝트 후원",
    "o.c3.d": "모든 기능은 비용 없이 쓸 수 있습니다. 돕고 싶다면 연결 방식으로 <a href=\"" + DNSPILOT + "\">DNSPilot</a>을 고려해 주세요. VCA를 만드는 팀이 개발하며, 유료 플랜이 열리면 구독료가 VCA 개발에 보탬이 됩니다. 어디까지나 선택 사항이며 Tailscale, Cloudflare Tunnel, LAN, 사용자 지정 방식과 똑같이 지원합니다. 기여하거나, 저장소에 별을 달고 널리 알려 주셔도 큰 힘이 됩니다.",
    "o.c3.l": "저장소에 별 달기",
    "o.credits": "VCA는 수많은 오픈소스 프로젝트 위에 만들어졌습니다. 그 프로젝트를 만든 모든 분께 감사드립니다.",
    "o.creditsLink": "오픈소스 크레딧 (영문)",
    "o.noticesLink": "서드파티 고지 (영문)",

    "footer.label": "푸터 메뉴",
    "footer.slogan": "말로 지휘하고, 흐름을 관리하다.",
    "footer.guide": "사용자 가이드",
    "footer.report": "초기 테스트 보고서",
    "footer.brand": "브랜드 가이드라인",
    "footer.credits": "오픈소스 크레딧",
    "footer.license": "MIT 라이선스",
    "footer.privacy": "추적, 분석 도구, 쿠키를 사용하지 않습니다.",
    "nav.download": "다운로드",
    "f16.t": "화면 배치",
    "f16.d": "PC에 이미 열려 있는 터미널 창을 모니터별로 가지런히 배치합니다. 1, 2, 4, 6, 8, 10칸 격자나 원하는 행과 열을 고르세요. 자동 채우기, 배치 저장과 다시 적용, 방금 한 이동 되돌리기를 지원합니다.",
    "f17.t": "앱 잠금",
    "f17.d": "앱은 잠긴 채로 열리고, Windows Hello, Touch ID, Face ID, 지문, 기기 암호처럼 기기 자체의 인증으로 잠금이 풀립니다. 잠겨 있는 동안에는 세션 내용이 보이지 않고 아무것도 보낼 수 없습니다. 1분에서 60분 동안 쓰지 않으면 다시 잠깁니다.",
    "f18.t": "자주 틀리는 단어",
    "f18.d": "받아쓰기가 자꾸 틀리는 단어를 알려 주세요. 올바른 단어와 잘못 적히는 형태를 함께 등록하면 됩니다. 목록은 PC에 저장되므로 다음 받아쓰기부터 연결된 모든 기기에 적용됩니다.",
    "p.kindWin": "서버와 데스크톱 앱 · MetaVu Co., Ltd. 서명",
    "p.rel": "GitHub Releases",
    "p.appStore": "App Store · 심사 중",
    "p.play": "Google Play",
    "dl.eyebrow": "다운로드",
    "dl.title": "Voice Command AI 0.1.1을 받으세요.",
    "dl.intro": "에이전트가 실행되는 PC에 데스크톱 앱을 설치하세요. VCA 서버와 vca 명령어가 함께 들어 있고, GitHub Releases에서 스스로 업데이트합니다. 직접 누르기 전에는 아무것도 설치하지 않습니다. 그다음 설정 › 연결·보안 › 휴대폰 연결의 QR 코드 하나로 휴대폰을 페어링하세요.",
    "dl.win": "설치 파일(-setup.exe), MetaVu Co., Ltd. 서명",
    "dl.mac": "디스크 이미지(.dmg), 서명 및 공증 완료",
    "dl.linux": "x86_64와 ARM64용 .deb, .rpm 패키지를 제공합니다.",
    "dl.cta": "GitHub Releases에서 전체 파일과 릴리스 노트 보기",
    "footer.privacyLink": "개인정보 처리방침 (영문)",
    "footer.support": "지원 (영문)",
    "dl.btnWin": "Windows (x64)",
    "dl.btnMacArm": "macOS (Apple 실리콘)",
    "dl.btnMacIntel": "macOS (Intel)",
    "dl.btnLinux": "Linux (x86_64, ARM64)",
    "v.title": "2분 소개 영상 보기",
    "v.meta": "YouTube 영상 (2:24, 한국어). 새 탭에서 열립니다.",
    "v.url": "https://youtu.be/I3vdZhCmwQg"
  };

  var JA = {
    "meta.title": "Voice Command AI · 声で指揮し、流れを管理する。",
    "meta.desc": "PC上のAIコーディングエージェントを、スマートフォンから声で確認・指示できるオープンソースツールです。Claude Code、Codex、Gemini CLIなどに対応し、エンドツーエンド暗号化とMITライセンスを採用しています。",
    "meta.ogTitle": "Voice Command AI: 声で指揮し、流れを管理する。",
    "meta.ogDesc": "PC上のすべてのAIコーディングエージェントを、スマートフォンから声で指揮。オープンソース、MITライセンス。",

    "skip": "本文へスキップ",
    "brand.home": "Voice Command AI ホーム",
    "nav.label": "メインメニュー",
    "nav.why": "VCAを選ぶ理由",
    "nav.features": "機能",
    "nav.how": "仕組み",
    "nav.measured": "検証結果",
    "nav.oss": "オープンソース",
    "lang.label": "言語",
    "gh.aria": "GitHubのVoice Command AI",

    "hero.symbolAlt": "Voice Command AI シンボル",
    "hero.badge": "バージョン0.1.1 · Linuxに対応",
    "hero.title": "<span class=\"line\">声で指揮し、</span> <span class=\"line\">流れを管理する。</span>",
    "hero.lead": "PC上のすべてのAIコーディングエージェントを、スマートフォンから声で指揮しましょう。",
    "hero.sub": "PCで動く軽量なサーバーが、すべてのエージェントセッションを管理します。スマートフォンにはセッションがカードで並び、コマンドはウィンドウのフォーカスを奪わずに正しいセッションへ届きます。",
    "cta.github": "GitHubで見る",
    "cta.guide": "ユーザーガイド (英語)",
    "hero.meta1": "MITライセンス",
    "hero.meta2": "エンドツーエンド暗号化",
    "hero.meta3": "自分のPCで動作、VCAクラウドなし",

    "g.alt": "VCAアプリのイメージ: 3枚のセッションカードと、api-serverセッション宛ての音声コマンド",
    "g.conn": "接続済み · 暗号化",
    "g.run": "実行中",
    "g.wait": "入力待ち",
    "g.done": "完了",
    "g.msg1": "テストスイートを実行中…",
    "g.msg2": "データベースのマイグレーションを適用しますか？",
    "g.msg3": "テスト完了。失敗した2件を確認してください。",
    "g.to": "送信先",
    "g.text": "結合テストを実行して、失敗した項目だけ要約して。",
    "g.send": "送信",
    "g.res": "結合テスト完了。失敗2件を要約しました。",
    "g.caption": "イメージ図です。送信する前に、どのセッションに届くかを表示します。",

    "why.eyebrow": "VCAを選ぶ理由",
    "why.title": "エージェントは多数、キーボードは一つ。",
    "why.intro": "エージェントセッションを並行して動かすと、サブエージェント、コンパイラ、テストランナー、補助ウィンドウが1台のPCに集まります。最初に崩れるのは次の3つです。",
    "why.with": "VCAなら",
    "why.p1.title": "フォーカスの奪い合い",
    "why.p1.body": "新しいウィンドウが次々にフォーカスを奪い、プロンプトやキー入力が別のターミナルに入ってしまいます。",
    "why.p1.fix": "仮想ターミナルを通じて、対象のセッションに直接コマンドを届けます。キー入力のシミュレーションは使いません。",
    "why.p2.title": "リソースの枯渇",
    "why.p2.body": "エージェントが互いを知らないままビルドやテストを始め、CPUとメモリを使い切ってPCが止まります。",
    "why.p2.fix": "リソースブローカーが重いジョブを決められたスロット数だけ実行し、残りは待ち行列に入れます。",
    "why.p3.title": "画面移動の手間",
    "why.p3.body": "既存のリモート操作アプリでは、コマンドを送るたびにプロジェクトに入っては戻る操作を繰り返します。",
    "why.p3.fix": "すべてのセッションが1組のカードに並びます。選んで、話して、送るだけです。",

    "feat.eyebrow": "機能",
    "feat.title": "今のVCAでできること",
    "feat.intro": "「近日公開」の項目を除き、以下の機能はすべてバージョン0.1.1に含まれています。",
    "soon": "近日公開",
    "f1.t": "セッションカード",
    "f1.d": "PC上のすべてのエージェントセッションをライブカードで表示。プロジェクト、エージェント、状態、通知がひと目でわかります。すでに開いているAI CLIセッションを取り込んだり、選んだフォルダーで新しいセッションを始めたりできます。カードには、そのセッションが動かしているサブエージェントの数も表示されます。",
    "f2.t": "音声ファースト",
    "f2.d": "タップして話すだけ。話すそばから文字起こしが表示され、内容を確認してから送信します。開発者向け用語エンジンが技術用語を正しく保ちます。",
    "f3.t": "フォーカス不要",
    "f3.d": "コマンドは仮想ターミナルかエージェント自身のチャネルで対象のセッションに届きます。キー入力のシミュレーションは使いません。",
    "f4.t": "ライブビュー",
    "f4.d": "どのセッションも、読みやすいチャット表示か生のターミナルで追いかけ、そのまま会話を続けられます。チャット検索、Markdownの表、ページ単位で読み込む過去の履歴にも対応しています。",
    "f5.t": "通知、必要なら正確に",
    "f5.d": "独立した通知メニューが、判断や入力を待つセッションを件数バッジ付きで一覧表示します。設定からインストールできる任意のエージェントフックを使うと、承認の通知が正確になります。返答はワンタップで。",
    "f6.t": "自分の音声エンジンで読み上げ",
    "f6.d": "自分で用意したOpenAI互換のTTSエンジンで通知を聞けます。MeloTTSやSupertonic用のリファレンスサーバーもあります。PCの音声を選ぶと、読み上げはすべてその声で一つずつ流れ、オン・オフはワンタップ。端末の音声は、PCの音声が始まる前だけの代わりです。",
    "f7.t": "エージェントからのやること",
    "f7.d": "サインイン、承認、コマンドの実行など、エージェントがあなたに頼みたいことは一つのリストにまとまります。チェックして完了にし、結果をセッションに送り返せます。コマンドが勝手に実行されることはありません。管理者権限が必要な作業は、OSの確認ダイアログを通してのみ管理者シェルを開きます。",
    "f8.t": "再起動後もウィンドウを復元",
    "f8.d": "再起動や停電のあとも、AI CLIのウィンドウを同じ位置、同じ会話で開き直します。Windows、macOS、Linux、tmuxに対応しています。",
    "f9.t": "しっかり守られたリモートシェル",
    "f9.d": "エージェントが行き詰まったら、スマートフォンからPCのシェルを開けます。開けるのは管理者権限でペアリングした端末だけ。シェルごとにFace IDなどの生体認証と新しい認証コードが必要で、時間が来ると自動で閉じます。",
    "f10.t": "スマートフォンのための設計",
    "f10.d": "セッションをカルーセルでめくり、写真、ファイル、手描きのスケッチを添付し、ライト、ダーク、真っ黒なブラックのテーマから選べます。PCがまだなくても、サンプルのセッションでデモを試せます（オフライン）。日本語、英語、韓国語、スペイン語に対応しています。",
    "f11.t": "リソースブローカー",
    "f11.d": "セッションごとのCPU、メモリ、ディスク、ネットワークを監視します。重いジョブはポリシーに従って実行、待機、制限、一時停止されます。",
    "f12.t": "ペアリングと暗号化",
    "f12.d": "使い捨てのペアリングリンク、TOTPによる2段階認証、短命なトークン、Noiseによるエンドツーエンド暗号化、監査ログ、キルスイッチを備えています。",
    "f13.t": "自分のPC、自分の接続方法",
    "f13.d": "自宅、オフィス、サーバーのPCを一つのアプリでペアリング。LAN直接接続、Tailscale、Cloudflare Tunnel、DNSPilot、または独自のプロバイダーで接続します。接続方法を選ぶまで、サーバーはlocalhostでのみ待ち受けます。",
    "f14.t": "エンジンは自分で選ぶ",
    "f14.d": "音声認識(たとえばWhisperサーバー)、補正用LLM、音声合成を、ローカルでも自分のクラウドアカウントでも自由に選べます。VCAは音声サーバーもモデルの重みも同梱しません。",
    "f15.t": "アプリを閉じているときのスマートフォン通知",
    "f15.d": "今回のリリースには含まれておらず、今後対応する予定です。現在、スマートフォンではVCAアプリを開いている間に通知を受け取り、デスクトップではバックグラウンドでもシステム通知が表示されます。",

    "c.eyebrow": "司令室",
    "c.title": "すべてのエージェントを見守る指揮セッション。",
    "c.intro": "任意の指揮セッションをオンにして、「対応が必要なセッションは？」「apiセッションにテストを実行するよう伝えて」のように話しかけましょう。VCAが把握している情報で答え、VCAのMCPツールで動きますが、実行するのはあなたが許可したあとだけです。",
    "c.l1": "他のセッションへのメッセージは、すべてあなたの許可か拒否を待ちます。",
    "c.l2": "権限の承認には決して答えません。その判断はいつもあなたのものです。",
    "c.l3": "操作の回数には1時間あたりの上限があり、既定は20回です。",
    "c.l4": "緊急停止で止めると、保留中の依頼もすべて拒否されます。",
    "c.l5": "すべての操作が監査ログに記録されます。",
    "c.runs": "使えるエージェント",
    "c.builtin": "内蔵 (自分のLLM)",
    "c.note": "既定ではオフで、オンにできるのはPC本体のデスクトップアプリだけです。",
    "c.link": "指揮セッションの詳細 (英語)",
    "c.alt": "司令室の画面イメージ: 指揮セッションがapi-serverセッションへのメッセージ送信を求め、許可か拒否を待っています。",
    "c.p.title": "司令室",
    "c.p.state": "あなたの返答待ち",
    "c.p.you": "api-serverに結合テストを実行するよう伝えて。",
    "c.p.req": "指揮セッションが <strong>api-server</strong> に次の内容を送ろうとしています:",
    "c.p.msg": "結合テストを実行して、失敗した項目を要約して。",
    "c.p.allow": "許可",
    "c.p.deny": "拒否",
    "c.p.timer": "2分以内に返答がなければ自動で拒否されます",
    "c.p.cap": "この1時間の操作",
    "c.p.kill": "緊急停止",

    "how.eyebrow": "仕組み",
    "how.title": "スマートフォンが話す相手は、自分のPCだけ。",
    "how.intro": "一度ペアリングすれば、アプリはNoiseによるエンドツーエンド暗号化チャネルでPCのVCAサーバーに接続します。エージェントセッションはサーバーが管理し、VCAのクラウドを経由するものは何もありません。",
    "d.alt": "構成図: スマートフォンやデスクトップのVCAアプリが、エンドツーエンド暗号化チャネルでPCのVCAサーバーに接続します。サーバーはClaude Code、Codex、Gemini CLI、Antigravity、Grokなどのエージェントを管理し、任意で自分の音声認識、LLM、音声合成エンジンを使えます。",
    "d.app": "VCAアプリ",
    "d.appSub": "スマートフォン · デスクトップ",
    "d.app1": "セッションカード",
    "d.app2": "音声入力",
    "d.app3": "ライブビューと通知",
    "d.e2e": "エンドツーエンド暗号化",
    "d.noise": "Noiseプロトコル",
    "d.via1": "LAN · Tailscale · Cloudflare",
    "d.via2": "DNSPilot · カスタム",
    "d.server": "VCAサーバー",
    "d.serverSub": "自分のPCで動作",
    "d.s1": "セッションマネージャー (PTY)",
    "d.s2": "エージェントアダプター",
    "d.s3": "リソースブローカー",
    "d.s4": "音声パイプライン",
    "d.s5": "認証 · 監査ログ · API",
    "d.agents": "PC上のエージェント",
    "d.more": "その他",
    "d.stt": "自分の音声エンジン (任意)",
    "d.sttSub": "STT · LLM · TTS、OpenAI互換",
    "d.caption": "青い枠は専用アダプターのあるエージェントです。その他は汎用アダプターで動作します。",
    "s1.t": "VCAの下でエージェントを実行",
    "s1.d": "管理セッションは、ウィンドウのフォーカスに関係なくアプリからの入力を受け付けます。",
    "s2.t": "ペアリングは一度だけ",
    "s2.d": "デスクトップアプリで「設定 › 接続・セキュリティ › スマートフォンを接続」を開き、VCAアプリでQRコードを読み取ります。1回の読み取りでペアリングが済み、DNSPilotを使う場合はリモートアクセスの登録も同時に済みます。ターミナルからは:",
    "s3.t": "カードを選んで話す",
    "s3.d": "セッションを選んで話し、送信先と内容を確かめてから送ります。",

    "m.eyebrow": "初期の測定結果",
    "m.title": "実機で測った数字です。",
    "m.intro": "マイルストーンM0では、2026年10月にWindows、macOS、Linux、iPhoneで各構成要素を一つずつ検証しました。主な結果は次のとおりです。",
    "m.s1": "LANとTailscaleでそれぞれ2時間連続で試したリクエストへの応答数。欠落0件",
    "m.v2": "1.3<small> 秒</small>",
    "m.s2": "サーバーの再起動から再接続までの時間。再ペアリングは不要",
    "m.v3": "~0.3<small> 秒</small>",
    "m.s3": "約10秒の音声をGPUのWhisperサーバーで文字起こしするのにかかった時間",
    "m.v4": "5.5<small> ms</small>",
    "m.s4": "暗号化チャネルを通したLAN往復遅延の中央値",
    "m.c1": "WindowsとmacOSで管理セッションを検証",
    "m.c2": "実際のClaude CodeフックとCodex入力を検証",
    "m.c3": "実際のcargoビルドで実行制御を検証",
    "m.c4": "iPhoneの音声入力から文字起こしまで検証",
    "m.note": "0.1.0のリリース前、2026年10月に最初のテストビルドで測定した結果です。<a href=\"" + REPORT + "\">テストレポート全文を読む (英語)</a>",
    "m.tts": "音声合成エンジンは別途比較しました。オープンソースの7エンジンに、同じフレーズを韓国語、英語、日本語、スペイン語で読ませています。<a href=\"" + TTS + "\">TTSベンチマークを読む (英語)</a>",

    "p.eyebrow": "対応範囲",
    "p.title": "使い慣れたエージェントをそのまま。",
    "p.agentsTitle": "エージェント",
    "p.agentsBody": "ターミナルで動くエージェントなら、どれも管理セッションとして実行できます。Claude CodeとCodexには専用アダプターがあり、その他は出力からプロンプトや待機状態を読み取る汎用アダプターで動作します。",
    "p.more": "その他",
    "p.legend": "専用アダプター",
    "p.tm": "製品名は各所有者の商標です。",
    "p.platTitle": "プラットフォーム",
    "p.platBody": "サーバーは開発用PCで、アプリはデスクトップとスマートフォンで動きます。デスクトップ版のインストーラーはGitHub Releasesで入手でき、AndroidアプリはGoogle Playで公開中、iPhoneアプリはApp Storeで審査中です。",
    "p.kindDesk": "サーバーとデスクトップアプリ",
    "p.kindMac": "サーバーとデスクトップアプリ · 署名・公証済み",
    "p.kindApp": "モバイルアプリ",
    "p.src": "ソースからビルド",
    "p.internal": "内部テストビルド",

    "t.eyebrow": "試してみる",
    "t.title": "ソースからビルドすることもできます。",
    "t.body": "ソースからサーバーとCLIをインストールし、サーバーを起動して、最初のエージェントをVCAの下で実行しましょう。",
    "t.reqs": "GitとRustツールチェーンが必要です。Windowsでは「C++によるデスクトップ開発」のビルドツールも入れてください。",
    "t.copy": "コピー",
    "t.copied": "コピーしました",
    "t.codeLabel": "クイックスタートのコマンド",

    "o.eyebrow": "オープンソース",
    "o.title": "MITライセンス。すべての機能が無料。",
    "o.intro": "コアはRust、アプリはTauri 2で作られています。どの機能も、誰にも支払わずに使えます。",
    "o.c1.t": "コードを読む",
    "o.c1.d": "サーバー、CLI、共通UI、デスクトップとモバイルのアプリが、MITライセンスの一つのリポジトリにあります。",
    "o.c1.l": "リポジトリを見る",
    "o.c2.t": "コントリビュート",
    "o.c2.d": "コード、エージェントアダプター、翻訳、あなたの言語の開発者向け用語集まで。IssueもPull Requestも歓迎します。",
    "o.c2.l": "Issueを開く",
    "o.c3.t": "プロジェクトを支援する",
    "o.c3.d": "すべての機能は無料で使えます。応援したい方は、接続方法として<a href=\"" + DNSPILOT + "\">DNSPilot</a>をご検討ください。VCAと同じチームが開発しており、有料プランが始まれば、その利用料がVCAの開発を支えます。あくまで任意で、Tailscale、Cloudflare Tunnel、LAN、独自のプロバイダーと同じように対応しています。コントリビュートや、リポジトリへのスター、ほかの開発者への紹介も大きな力になります。",
    "o.c3.l": "リポジトリにスターを付ける",
    "o.credits": "VCAは多くのオープンソースプロジェクトの成果の上に成り立っています。携わるすべての皆さんに感謝します。",
    "o.creditsLink": "オープンソースのクレジット (英語)",
    "o.noticesLink": "サードパーティーに関する告知 (英語)",

    "footer.label": "フッターメニュー",
    "footer.slogan": "声で指揮し、流れを管理する。",
    "footer.guide": "ユーザーガイド",
    "footer.report": "初期テストレポート",
    "footer.brand": "ブランドガイドライン",
    "footer.credits": "オープンソースのクレジット",
    "footer.license": "MITライセンス",
    "footer.privacy": "トラッキング、アクセス解析、Cookieは使っていません。",
    "nav.download": "ダウンロード",
    "f16.t": "画面配置",
    "f16.d": "PCですでに開いているターミナルウィンドウを、モニターごとに整列します。1、2、4、6、8、10マスのグリッドか、好きな行数と列数を選べます。自動配置、レイアウトの保存と再適用、直前の移動の取り消しに対応しています。",
    "f17.t": "アプリのロック",
    "f17.d": "アプリはロックされた状態で開き、Windows Hello、Touch ID、Face ID、指紋、デバイスのパスコードなど、デバイス自身の認証で解除します。ロック中はセッションの内容が表示されず、何も送信できません。1〜60分使わないと再びロックされます。",
    "f18.t": "よく聞き間違える単語",
    "f18.d": "音声入力が何度も間違える単語を登録できます。正しい単語と、誤って書かれる形を一緒に登録します。リストはPCに保存されるので、次の音声入力から接続中のすべてのデバイスに反映されます。",
    "p.kindWin": "サーバーとデスクトップアプリ · MetaVu Co., Ltd.の署名付き",
    "p.rel": "GitHub Releases",
    "p.appStore": "App Store · 審査中",
    "p.play": "Google Play",
    "dl.eyebrow": "ダウンロード",
    "dl.title": "Voice Command AI 0.1.1を入手する。",
    "dl.intro": "エージェントが動くPCにデスクトップアプリをインストールしてください。VCAサーバーとvcaコマンドが含まれ、GitHub Releasesから自動で更新されます。クリックするまで何もインストールされません。その後、「設定 › 接続・セキュリティ › スマートフォンを接続」のQRコード1つでスマートフォンをペアリングします。",
    "dl.win": "インストーラー（-setup.exe）、MetaVu Co., Ltd.の署名付き",
    "dl.mac": "ディスクイメージ（.dmg）、署名・公証済み",
    "dl.linux": "x86_64とARM64向けの.debと.rpmパッケージを提供しています。",
    "dl.cta": "すべてのファイルとリリースノート（GitHub Releases）",
    "footer.privacyLink": "プライバシーポリシー (英語)",
    "footer.support": "サポート (英語)",
    "dl.btnWin": "Windows (x64)",
    "dl.btnMacArm": "macOS（Appleシリコン）",
    "dl.btnMacIntel": "macOS（Intel）",
    "dl.btnLinux": "Linux（x86_64、ARM64）",
    "v.title": "2分の紹介動画を見る",
    "v.meta": "YouTubeの動画（2:10、英語）。新しいタブで開きます。",
    "v.url": "https://youtu.be/y1nxSgDrQi8"
  };

  var ES = {
    "meta.title": "Voice Command AI · Tu voz. Todos los agentes. Un solo centro de mando.",
    "meta.desc": "Herramienta de código abierto para ver y dirigir con la voz, desde el móvil, los agentes de programación con IA de tu equipo. Claude Code, Codex, Gemini CLI y más. Cifrado de extremo a extremo, licencia MIT.",
    "meta.ogTitle": "Voice Command AI: Tu voz. Todos los agentes. Un solo centro de mando.",
    "meta.ogDesc": "Dirige con la voz, desde el móvil, todos los agentes de programación con IA de tu equipo. Código abierto, licencia MIT.",

    "skip": "Saltar al contenido",
    "brand.home": "Inicio de Voice Command AI",
    "nav.label": "Principal",
    "nav.why": "Por qué VCA",
    "nav.features": "Funciones",
    "nav.how": "Cómo funciona",
    "nav.measured": "Mediciones",
    "nav.oss": "Código abierto",
    "lang.label": "Idioma",
    "gh.aria": "Voice Command AI en GitHub",

    "hero.symbolAlt": "Símbolo de Voice Command AI",
    "hero.badge": "Versión 0.1.1 · Ahora también en Linux",
    "hero.title": "<span class=\"line\">Tu voz.</span> <span class=\"line\">Todos los agentes.</span> <span class=\"line\">Un solo centro de mando.</span>",
    "hero.lead": "Dirige con la voz, desde el móvil, todos los agentes de programación con IA de tu equipo.",
    "hero.sub": "Un servidor ligero en tu equipo gestiona todas las sesiones de agentes. El móvil las muestra como tarjetas, y cada orden llega a la sesión correcta sin robar el foco de ninguna ventana.",
    "cta.github": "Ver en GitHub",
    "cta.guide": "Guía de uso (en inglés)",
    "hero.meta1": "Licencia MIT",
    "hero.meta2": "Cifrado de extremo a extremo",
    "hero.meta3": "Funciona en tu equipo, sin nube de VCA",

    "g.alt": "Ilustración de la app VCA: tres tarjetas de sesión y una orden dictada para la sesión api-server.",
    "g.conn": "Conectado · cifrado",
    "g.run": "En curso",
    "g.wait": "Espera respuesta",
    "g.done": "Listo",
    "g.msg1": "Ejecutando las pruebas…",
    "g.msg2": "¿Aplico la migración de la base de datos?",
    "g.msg3": "Pruebas terminadas. Revisa los 2 fallos.",
    "g.to": "Para",
    "g.text": "Ejecuta las pruebas de integración y resume los fallos.",
    "g.send": "Enviar",
    "g.res": "Pruebas de integración listas. 2 fallos resumidos.",
    "g.caption": "Ilustración. La sesión de destino se muestra antes de enviar nada.",

    "why.eyebrow": "Por qué VCA",
    "why.title": "Muchos agentes. Un teclado. Algo se rompe.",
    "why.intro": "Las sesiones de agentes en paralelo llenan un mismo equipo de subagentes, compiladores, ejecutores de pruebas y ventanas auxiliares. Tres cosas fallan primero.",
    "why.with": "Con VCA",
    "why.p1.title": "Robo de foco",
    "why.p1.body": "Las ventanas nuevas no dejan de robar el foco, y las instrucciones y pulsaciones acaban en el terminal equivocado.",
    "why.p1.fix": "Las órdenes van directas a la sesión de destino a través de un terminal virtual. Sin pulsaciones simuladas.",
    "why.p2.title": "Recursos agotados",
    "why.p2.body": "Los agentes lanzan compilaciones y pruebas sin saber unos de otros, hasta saturar la CPU y la memoria y bloquear el equipo.",
    "why.p2.fix": "Un gestor de recursos admite los trabajos pesados en un número fijo de plazas y pone el resto en cola.",
    "why.p3.title": "Demasiada navegación",
    "why.p3.body": "Las apps de control remoto te obligan a entrar en un proyecto, salir y entrar en el siguiente para cada orden.",
    "why.p3.fix": "Todas las sesiones están en un mismo mazo de tarjetas. Elige una, habla y envía.",

    "feat.eyebrow": "Funciones",
    "feat.title": "Lo que VCA ya hace",
    "feat.intro": "Todo lo que sigue está en la versión 0.1.1, salvo lo marcado como «Próximamente».",
    "soon": "Próximamente",
    "f1.t": "Tarjetas de sesión",
    "f1.d": "Cada sesión de agente de tu equipo como una tarjeta en vivo: proyecto, agente, estado y alertas de un vistazo. Importa sesiones de CLI de IA que ya estén abiertas o inicia una en la carpeta que elijas. Cada tarjeta cuenta también los subagentes que ejecuta su sesión.",
    "f2.t": "Primero, la voz",
    "f2.d": "Toca y habla. El dictado aparece en directo mientras hablas; revisa el texto y envíalo. Un motor de vocabulario para desarrolladores mantiene intactos los términos técnicos.",
    "f3.t": "Sin necesidad de foco",
    "f3.d": "Las órdenes llegan a la sesión de destino por un terminal virtual o por el canal propio del agente, nunca con pulsaciones simuladas.",
    "f4.t": "Vista en directo",
    "f4.d": "Sigue cualquier sesión como un chat legible o como el terminal original, y continúa la conversación desde ahí. Busca en el chat, lee tablas de Markdown y recorre el historial, que se carga por páginas.",
    "f5.t": "Alertas, exactas si lo deseas",
    "f5.d": "Un menú de Alertas propio, con un contador, reúne las sesiones que esperan una decisión o una respuesta. Los hooks opcionales para agentes, que instalas desde Ajustes, hacen exactas las alertas de aprobación. Responde con un toque.",
    "f6.t": "Alertas habladas con tu motor de voz",
    "f6.d": "Escucha las alertas con tu propio motor TTS compatible con OpenAI, por ejemplo el servidor de referencia para MeloTTS o Supertonic. Elige una voz del PC y todo lo que se dice la usa, de uno en uno y con un interruptor rápido. La voz del dispositivo solo la sustituye antes de que empiece el audio del PC.",
    "f7.t": "Tareas que te piden los agentes",
    "f7.d": "Cuando un agente necesita que inicies sesión, apruebes o ejecutes algo, la petición llega a una sola lista. Márcala como hecha y envía el resultado de vuelta. Nada se ejecuta por ti: las tareas de administrador abren un shell elevado solo a través del aviso de tu sistema operativo.",
    "f8.t": "Tus ventanas, tras reiniciar",
    "f8.d": "Después de un reinicio o un corte de luz, VCA vuelve a abrir tus ventanas de CLI de IA en el mismo sitio y en la misma conversación. En Windows, macOS, Linux y tmux.",
    "f9.t": "Shell remoto, bien protegido",
    "f9.d": "Cuando un agente se atasca, abre un shell en tu PC desde el móvil. Solo pueden hacerlo los dispositivos con permiso de administrador, cada shell pide Face ID u otra biometría y un código nuevo del autenticador, y se cierra solo al cumplirse un tiempo límite.",
    "f10.t": "Pensado para el móvil",
    "f10.d": "Desliza entre sesiones en un carrusel, adjunta fotos, archivos o un boceto rápido, y elige un tema claro, oscuro o negro puro. ¿Aún sin PC? Prueba la demo con sesiones de ejemplo, sin conexión. En español, inglés, coreano y japonés.",
    "f11.t": "Gestor de recursos",
    "f11.d": "CPU, memoria, disco y red por sesión. Los trabajos pesados se admiten, se ponen en cola, se limitan o se pausan según tus políticas.",
    "f12.t": "Emparejamiento y cifrado",
    "f12.d": "Enlaces de emparejamiento de un solo uso, inicio de sesión en dos pasos con TOTP, tokens de corta duración, cifrado de extremo a extremo con Noise, registro de auditoría y botón de emergencia.",
    "f13.t": "Tus equipos, tu conexión",
    "f13.d": "Empareja con una sola app los equipos de casa, de la oficina y tus servidores. Conéctate por LAN directa, Tailscale, Cloudflare Tunnel, DNSPilot o tu propio proveedor. El servidor solo escucha en localhost hasta que eliges uno.",
    "f14.t": "Tus propios motores",
    "f14.d": "Tú eliges la transcripción de voz (por ejemplo, un servidor Whisper), el LLM de corrección y la síntesis de voz, en local o en tu propia cuenta en la nube. VCA no incluye servidores de voz ni pesos de modelos.",
    "f15.t": "Alertas en el móvil con la app cerrada",
    "f15.d": "Previsto, no incluido en esta versión. Hoy, los móviles muestran alertas mientras la app de VCA está abierta, y los equipos de escritorio muestran notificaciones del sistema en segundo plano.",

    "c.eyebrow": "Centro de mando",
    "c.title": "Un director para todos tus agentes.",
    "c.intro": "Activa una sesión directora opcional y pregúntale «¿qué sesiones me necesitan?» o «dile a la sesión api que ejecute las pruebas». Responde con lo que ve VCA y actúa con las herramientas MCP de VCA, pero solo después de que lo permitas.",
    "c.l1": "Cada mensaje a otra sesión espera a que pulses Permitir o Denegar.",
    "c.l2": "Nunca responde a las solicitudes de permiso. Esas siguen siendo tuyas.",
    "c.l3": "Las acciones tienen un límite por hora, 20 por defecto.",
    "c.l4": "Un botón de emergencia lo detiene y deniega todas las peticiones pendientes.",
    "c.l5": "Cada acción queda en el registro de auditoría.",
    "c.runs": "Funciona con",
    "c.builtin": "Integrado, con tu propio LLM",
    "c.note": "Desactivado por defecto, y solo se activa desde la app de escritorio en el propio PC.",
    "c.link": "Cómo funciona el director (en inglés)",
    "c.alt": "Ilustración de la vista Mando: el director pide enviar un mensaje a la sesión api-server y espera a que pulses Permitir o Denegar.",
    "c.p.title": "Mando",
    "c.p.state": "Te está esperando",
    "c.p.you": "Dile a api-server que ejecute las pruebas de integración.",
    "c.p.req": "El director quiere enviar esto a <strong>api-server</strong>:",
    "c.p.msg": "Ejecuta las pruebas de integración y resume los fallos.",
    "c.p.allow": "Permitir",
    "c.p.deny": "Denegar",
    "c.p.timer": "Se deniega solo si no respondes en 2 minutos",
    "c.p.cap": "Acciones en esta hora",
    "c.p.kill": "Botón de emergencia",

    "how.eyebrow": "Cómo funciona",
    "how.title": "Tu móvil solo habla con tu equipo.",
    "how.intro": "Tras un emparejamiento único, la app se conecta al servidor VCA de tu equipo por un canal Noise cifrado de extremo a extremo. El servidor gestiona las sesiones de los agentes. Nada pasa por una nube de VCA.",
    "d.alt": "Arquitectura: la app VCA del móvil o del escritorio se conecta por un canal cifrado de extremo a extremo al servidor VCA de tu equipo, que gestiona agentes como Claude Code, Codex, Gemini CLI, Antigravity y Grok, y puede usar tus propios motores opcionales de transcripción, LLM y síntesis de voz.",
    "d.app": "App VCA",
    "d.appSub": "Móvil · escritorio",
    "d.app1": "Tarjetas de sesión",
    "d.app2": "Dictado por voz",
    "d.app3": "Vista en directo y alertas",
    "d.e2e": "Cifrado de extremo a extremo",
    "d.noise": "Protocolo Noise",
    "d.via1": "LAN · Tailscale · Cloudflare",
    "d.via2": "DNSPilot · propio",
    "d.server": "Servidor VCA",
    "d.serverSub": "en tu equipo",
    "d.s1": "Gestor de sesiones (PTY)",
    "d.s2": "Adaptadores de agentes",
    "d.s3": "Gestor de recursos",
    "d.s4": "Canal de voz",
    "d.s5": "Autenticación · auditoría · API",
    "d.agents": "Agentes en tu equipo",
    "d.more": "y más",
    "d.stt": "Tus motores de voz (opcional)",
    "d.sttSub": "STT · LLM · TTS, compatibles con OpenAI",
    "d.caption": "El borde azul indica los agentes con adaptador propio. Los demás usan el adaptador genérico.",
    "s1.t": "Ejecuta tus agentes con VCA",
    "s1.d": "Las sesiones gestionadas aceptan entradas de la app sin tocar el foco de las ventanas.",
    "s2.t": "Empareja una vez",
    "s2.d": "En la app de escritorio, abre Ajustes › Conexión y seguridad › Conectar un teléfono y escanea su código QR con la app de VCA. Un solo escaneo empareja el teléfono y, con DNSPilot, también lo registra para el acceso remoto. Desde un terminal:",
    "s3.t": "Elige una tarjeta y habla",
    "s3.d": "Elige la sesión, dicta, comprueba el destino y el texto, y envía.",

    "m.eyebrow": "Primeras mediciones",
    "m.title": "Cifras de equipos reales.",
    "m.intro": "En el hito M0, en octubre de 2026, se probó cada pieza en Windows, macOS, Linux y un iPhone. Algunos resultados:",
    "m.s1": "peticiones respondidas en una prueba de 2 horas por LAN y por Tailscale, sin perder ninguna",
    "m.v2": "1,3<small> s</small>",
    "m.s2": "para reconectar tras reiniciar el servidor, sin volver a emparejar",
    "m.v3": "~0,3<small> s</small>",
    "m.s3": "para transcribir unos 10 s de voz en un servidor Whisper con GPU",
    "m.v4": "5,5<small> ms</small>",
    "m.s4": "de mediana en un viaje de ida y vuelta por LAN a través del canal cifrado",
    "m.c1": "Sesiones gestionadas verificadas en Windows y macOS",
    "m.c2": "Hooks reales de Claude Code y entrada a Codex",
    "m.c3": "Control de admisión con compilaciones reales de cargo",
    "m.c4": "Del dictado en el iPhone a la transcripción",
    "m.note": "Medido con las primeras versiones de prueba en octubre de 2026, antes de la versión 0.1.0. <a href=\"" + REPORT + "\">Leer el informe de pruebas completo (en inglés)</a>",
    "m.tts": "Los motores de síntesis de voz se compararon aparte: siete motores de código abierto, las mismas frases en coreano, inglés, japonés y español. <a href=\"" + TTS + "\">Leer la comparativa de TTS (en inglés)</a>",

    "p.eyebrow": "Compatible con",
    "p.title": "Trae los agentes que ya usas.",
    "p.agentsTitle": "Agentes",
    "p.agentsBody": "Cualquier agente de terminal puede ejecutarse como sesión gestionada. Claude Code y Codex tienen adaptadores propios; los demás usan un adaptador genérico que lee en su salida las preguntas y los estados de espera.",
    "p.more": "y más",
    "p.legend": "Adaptador propio",
    "p.tm": "Los nombres de productos son marcas de sus respectivos propietarios.",
    "p.platTitle": "Plataformas",
    "p.platBody": "El servidor funciona en tu equipo de desarrollo, y la app, en el escritorio y en el móvil. Los instaladores de escritorio están en GitHub Releases, la app de Android está en Google Play y la de iPhone está en revisión en la App Store.",
    "p.kindDesk": "Servidor y app de escritorio",
    "p.kindMac": "Servidor y app de escritorio · firmada y notarizada",
    "p.kindApp": "App móvil",
    "p.src": "Compilar desde el código",
    "p.internal": "Versiones de prueba internas",

    "t.eyebrow": "Pruébalo",
    "t.title": "O compílalo desde el código fuente.",
    "t.body": "Instala el servidor y la CLI desde el código fuente, inicia el servidor y ejecuta tu primer agente con VCA.",
    "t.reqs": "Necesitas Git y la cadena de herramientas de Rust. En Windows, también las herramientas de compilación «Desarrollo para el escritorio con C++».",
    "t.copy": "Copiar",
    "t.copied": "Copiado",
    "t.codeLabel": "Comandos de inicio rápido",

    "o.eyebrow": "Código abierto",
    "o.title": "Licencia MIT. Todas las funciones, gratis.",
    "o.intro": "El núcleo está escrito en Rust y las apps se crean con Tauri 2. Todas las funciones se pueden usar sin pagar a nadie.",
    "o.c1.t": "Lee el código",
    "o.c1.d": "El servidor, la CLI, la interfaz compartida y las apps de escritorio y móvil están en un solo repositorio con licencia MIT.",
    "o.c1.l": "Ver el repositorio",
    "o.c2.t": "Contribuye",
    "o.c2.d": "Código, adaptadores de agentes, traducciones o vocabulario de desarrollo en tu idioma. Las issues y las pull requests son bienvenidas.",
    "o.c2.l": "Abrir una issue",
    "o.c3.t": "Apoya el proyecto",
    "o.c3.d": "Todas las funciones se usan sin pagar a nadie. Si quieres ayudar, considera <a href=\"" + DNSPILOT + "\">DNSPilot</a> para la conexión: lo desarrolla el mismo equipo que VCA y, cuando abran sus planes de pago, las suscripciones ayudarán a financiar VCA. Es opcional y está al mismo nivel que Tailscale, Cloudflare Tunnel, la LAN y los proveedores propios. También puedes contribuir, o dar una estrella al repositorio y compartirlo.",
    "o.c3.l": "Dar una estrella al repositorio",
    "o.credits": "VCA se apoya en el trabajo de muchos proyectos de código abierto. Gracias a todas las personas que los hacen posibles.",
    "o.creditsLink": "Créditos de código abierto (en inglés)",
    "o.noticesLink": "Avisos de terceros (en inglés)",

    "footer.label": "Pie de página",
    "footer.slogan": "Tu voz. Todos los agentes. Un solo centro de mando.",
    "footer.guide": "Guía de uso",
    "footer.report": "Informe de las primeras pruebas",
    "footer.brand": "Guía de marca",
    "footer.credits": "Créditos de código abierto",
    "footer.license": "Licencia MIT",
    "footer.privacy": "Sin seguimiento, sin analítica, sin cookies.",
    "nav.download": "Descargar",
    "f16.t": "Disposición de pantalla",
    "f16.d": "Ordena las ventanas de terminal que ya tienes abiertas en el PC, monitor por monitor, en una cuadrícula de 1, 2, 4, 6, 8 o 10 celdas o con tus propias filas y columnas. Relleno automático, disposiciones guardadas que puedes volver a aplicar y deshacer el último movimiento.",
    "f17.t": "Bloqueo de la app",
    "f17.d": "Las apps se abren bloqueadas y se desbloquean con el inicio de sesión del propio dispositivo: Windows Hello, Touch ID, Face ID, huella o código. Mientras están bloqueadas no se ve el contenido de las sesiones ni se puede enviar nada. Se vuelven a bloquear tras 1 a 60 minutos sin uso.",
    "f18.t": "Palabras que se reconocen mal",
    "f18.d": "Enseña al dictado las palabras que siempre escribe mal: cada palabra correcta con lo que se escribe por error. La lista se guarda en tu PC, así que funciona en todos los dispositivos conectados desde el siguiente dictado.",
    "p.kindWin": "Servidor y app de escritorio · firmada por MetaVu Co., Ltd.",
    "p.rel": "GitHub Releases",
    "p.appStore": "App Store · en revisión",
    "p.play": "Google Play",
    "dl.eyebrow": "Descargar",
    "dl.title": "Consigue Voice Command AI 0.1.1.",
    "dl.intro": "Instala la app de escritorio en el PC donde se ejecutan tus agentes. Incluye el servidor de VCA y el comando vca, y se actualiza sola desde GitHub Releases; no se instala nada hasta que haces clic. Después, empareja el teléfono con un solo código QR desde Ajustes › Conexión y seguridad › Conectar un teléfono.",
    "dl.win": "Instalador (-setup.exe), firmado por MetaVu Co., Ltd.",
    "dl.mac": "Imagen de disco (.dmg), firmada y notarizada.",
    "dl.linux": "Paquetes .deb y .rpm para x86_64 y ARM64.",
    "dl.cta": "Todos los archivos y las notas de la versión en GitHub Releases",
    "footer.privacyLink": "Privacidad (en inglés)",
    "footer.support": "Soporte (en inglés)",
    "dl.btnWin": "Windows (x64)",
    "dl.btnMacArm": "macOS (Apple silicon)",
    "dl.btnMacIntel": "macOS (Intel)",
    "dl.btnLinux": "Linux (x86_64, ARM64)",
    "v.title": "Mira el recorrido de 2 minutos",
    "v.meta": "Vídeo en YouTube (2:10, en inglés). Se abre en una pestaña nueva.",
    "v.url": "https://youtu.be/y1nxSgDrQi8"
  };

  var ZH = {
    "meta.title": "Voice Command AI · 你的声音。所有智能体。一个指挥中心。",
    "meta.desc": "开源工具：在手机上用语音查看和指挥你电脑上的 AI 编程智能体。支持 Claude Code、Codex、Gemini CLI 等，端到端加密，MIT 许可证。",
    "meta.ogTitle": "Voice Command AI：你的声音。所有智能体。一个指挥中心。",
    "meta.ogDesc": "在手机上用语音指挥你电脑上的所有 AI 编程智能体。开源，MIT 许可证。",

    "skip": "跳到正文",
    "brand.home": "Voice Command AI 首页",
    "nav.label": "主菜单",
    "nav.why": "为什么选 VCA",
    "nav.features": "功能",
    "nav.how": "工作原理",
    "nav.measured": "实测结果",
    "nav.oss": "开源",
    "lang.label": "语言",
    "gh.aria": "GitHub 上的 Voice Command AI",

    "hero.symbolAlt": "Voice Command AI 标志",
    "hero.badge": "0.1.1 版 · 新增 Linux",
    "hero.title": "<span class=\"line\">你的声音。</span> <span class=\"line\">所有智能体。</span> <span class=\"line\">一个指挥中心。</span>",
    "hero.lead": "在手机上用语音指挥你电脑上的每一个 AI 编程智能体。",
    "hero.sub": "电脑上运行的轻量服务器接管所有智能体会话。手机把它们显示为卡片，每条指令都会送达正确的会话，而且不会抢走任何窗口的焦点。",
    "cta.github": "在 GitHub 上查看",
    "cta.guide": "用户指南（英文）",
    "hero.meta1": "MIT 许可证",
    "hero.meta2": "端到端加密",
    "hero.meta3": "运行在你的电脑上，不经过 VCA 云",

    "g.alt": "VCA 应用示意图：三张会话卡片，以及一条发给 api-server 会话的语音指令",
    "g.conn": "已连接 · 已加密",
    "g.run": "运行中",
    "g.wait": "等待输入",
    "g.done": "已完成",
    "g.msg1": "正在运行测试套件…",
    "g.msg2": "要执行数据库迁移吗？",
    "g.msg3": "测试已完成，请查看 2 个失败项。",
    "g.to": "发送到",
    "g.text": "运行集成测试，只总结失败的部分。",
    "g.send": "发送",
    "g.res": "集成测试完成，已总结 2 个失败项。",
    "g.caption": "示意图。发送之前会先显示目标会话。",

    "why.eyebrow": "为什么选 VCA",
    "why.title": "智能体很多，键盘只有一个。",
    "why.intro": "并行运行多个智能体会话时，子智能体、编译器、测试运行器和辅助窗口都挤在同一台电脑上。最先出问题的是这三件事。",
    "why.with": "VCA 的做法",
    "why.p1.title": "焦点被抢",
    "why.p1.body": "新窗口不断抢走焦点，提示词和按键被输入到错误的终端里。",
    "why.p1.fix": "通过虚拟终端把指令直接送到目标会话，不模拟任何按键。",
    "why.p2.title": "资源耗尽",
    "why.p2.body": "智能体彼此不知情地启动构建和测试，直到 CPU 和内存被占满，电脑卡死。",
    "why.p2.fix": "资源代理只让固定数量的重任务同时运行，其余的排队等待。",
    "why.p3.title": "来回切换",
    "why.p3.body": "现有的远程控制应用每发一条指令，都要进入一个项目、退出、再进入下一个。",
    "why.p3.fix": "所有会话都在同一组卡片里。选一张，说话，发送。",

    "feat.eyebrow": "功能",
    "feat.title": "VCA 现在能做什么",
    "feat.intro": "除标有“即将推出”的一项外，以下功能均已包含在 0.1.1 版中。",
    "soon": "即将推出",
    "f1.t": "会话卡片",
    "f1.d": "电脑上的每个智能体会话都是一张实时卡片：项目、智能体、状态和提醒一目了然。可以导入已经打开的 AI CLI 会话，也可以在你选择的文件夹里新建会话。每张卡片还会显示该会话运行的子智能体数量。",
    "f2.t": "语音优先",
    "f2.d": "点一下就说。听写内容随说随显示；确认文字后再发送。开发者词汇引擎会保持技术术语准确无误。",
    "f3.t": "无需焦点",
    "f3.d": "指令通过虚拟终端或智能体自己的通道送达目标会话，绝不模拟按键。",
    "f4.t": "实时查看",
    "f4.d": "以易读的聊天视图或原始终端跟进任意会话，并在其中继续对话。支持聊天搜索、Markdown 表格，以及按页加载的历史记录。",
    "f5.t": "提醒，可以做到精确",
    "f5.d": "独立的提醒菜单带有计数徽标，列出所有等待决定或输入的会话。在设置中安装可选的智能体钩子（hooks）后，审批提醒会变得精确。一键即可回复。",
    "f6.t": "用你自己的语音引擎播报",
    "f6.d": "通过你自己的 OpenAI 兼容 TTS 引擎收听提醒，例如适用于 MeloTTS 或 Supertonic 的参考服务器。选择电脑上的语音后，所有朗读都用这个声音，一次一条，可一键开关。设备自带语音只在电脑音频开始前临时代替。",
    "f7.t": "智能体交给你的待办",
    "f7.d": "当智能体需要你登录、审批或运行某个命令时，这些请求会汇集到一个列表里。勾选完成，再把结果发回会话。命令绝不会替你执行：需要管理员权限的任务只会通过操作系统的权限提示打开提权 shell。",
    "f8.t": "重启后恢复窗口",
    "f8.d": "重启或断电之后，VCA 会在原来的位置、原来的对话中重新打开你的 AI CLI 窗口。支持 Windows、macOS、Linux 和 tmux。",
    "f9.t": "严格保护的远程 shell",
    "f9.d": "智能体卡住时，可以从手机打开电脑上的 shell。只有以管理员权限配对的设备才能这样做，每个 shell 都需要 Face ID 或其他生物识别加上新的验证码，并会在时限到达后自动关闭。",
    "f10.t": "为手机而设计",
    "f10.d": "在轮播中滑动切换会话，附加照片、文件或随手草图，并可选择浅色、深色或纯黑主题。还没有电脑？可以用示例会话离线体验演示模式。应用界面支持英语、韩语、日语和西班牙语。",
    "f11.t": "资源代理",
    "f11.d": "按会话监控 CPU、内存、磁盘和网络。重任务会根据你的策略被放行、排队、限速或暂停。",
    "f12.t": "配对与加密",
    "f12.d": "一次性配对链接、TOTP 双因素登录、短期令牌、Noise 端到端加密、审计日志和紧急停止开关。",
    "f13.t": "你的电脑，你的连接方式",
    "f13.d": "用一个应用配对家里、办公室和服务器上的电脑。可通过局域网直连、Tailscale、Cloudflare Tunnel、DNSPilot 或你自己的方式访问。在你选定之前，服务器只监听 localhost。",
    "f14.t": "自带引擎",
    "f14.d": "语音识别（例如 Whisper 服务器）、纠错 LLM 和语音合成都由你选择，可以本地运行，也可以用你自己的云账号。VCA 不附带任何语音服务器或模型权重。",
    "f15.t": "应用关闭时的手机提醒",
    "f15.d": "已在计划中，本版本暂不包含。目前，手机在 VCA 应用打开时显示提醒，桌面端在后台也会显示系统通知。",

    "c.eyebrow": "指挥中心",
    "c.title": "为所有智能体配一位指挥。",
    "c.intro": "开启可选的指挥会话，问它“哪些会话在等我？”或“让 api 会话跑一下测试”。它根据 VCA 看到的信息回答，并通过 VCA 的 MCP 工具行动，但只在你允许之后才执行。",
    "c.l1": "发给其他会话的每条消息，都要等你选择允许或拒绝。",
    "c.l2": "它从不回应权限审批，这些决定始终由你来做。",
    "c.l3": "操作次数有每小时上限，默认 20 次。",
    "c.l4": "紧急停止会让它停下，并拒绝所有待处理的请求。",
    "c.l5": "每个操作都会写入审计日志。",
    "c.runs": "可用的智能体",
    "c.builtin": "内置，使用你自己的 LLM",
    "c.note": "默认关闭，只能在这台电脑的桌面应用中开启。",
    "c.link": "指挥会话的工作方式（英文）",
    "c.alt": "指挥室界面示意图：指挥会话请求向 api-server 会话发送一条消息，正在等待允许或拒绝。",
    "c.p.title": "指挥室",
    "c.p.state": "等待你的回复",
    "c.p.you": "让 api-server 运行集成测试。",
    "c.p.req": "指挥会话想向 <strong>api-server</strong> 发送以下内容：",
    "c.p.msg": "运行集成测试，并总结失败的部分。",
    "c.p.allow": "允许",
    "c.p.deny": "拒绝",
    "c.p.timer": "2 分钟内未回复将自动拒绝",
    "c.p.cap": "本小时操作",
    "c.p.kill": "紧急停止",

    "how.eyebrow": "工作原理",
    "how.title": "你的手机只和你的电脑通信。",
    "how.intro": "一次配对后，应用会通过 Noise 端到端加密通道连接到你电脑上的 VCA 服务器。智能体会话由服务器管理，任何内容都不经过 VCA 云。",
    "d.alt": "架构图：手机或桌面上的 VCA 应用通过端到端加密通道连接到你电脑上的 VCA 服务器。服务器管理 Claude Code、Codex、Gemini CLI、Antigravity、Grok 等智能体，并可选用你自己的语音识别、LLM 和语音合成引擎。",
    "d.app": "VCA 应用",
    "d.appSub": "手机 · 桌面",
    "d.app1": "会话卡片",
    "d.app2": "语音听写",
    "d.app3": "实时查看与提醒",
    "d.e2e": "端到端加密",
    "d.noise": "Noise 协议",
    "d.via1": "LAN · Tailscale · Cloudflare",
    "d.via2": "DNSPilot · 自定义",
    "d.server": "VCA 服务器",
    "d.serverSub": "运行在你的电脑上",
    "d.s1": "会话管理器（PTY）",
    "d.s2": "智能体适配器",
    "d.s3": "资源代理",
    "d.s4": "语音管线",
    "d.s5": "认证 · 审计 · API",
    "d.agents": "你电脑上的智能体",
    "d.more": "更多",
    "d.stt": "你的语音引擎（可选）",
    "d.sttSub": "STT · LLM · TTS，兼容 OpenAI",
    "d.caption": "蓝色边框表示有专用适配器的智能体，其余使用通用适配器。",
    "s1.t": "在 VCA 下运行智能体",
    "s1.d": "受管会话可以接收来自应用的输入，不影响窗口焦点。",
    "s2.t": "只需配对一次",
    "s2.d": "在桌面应用中打开 Settings › Connection & security › Connect a phone，用 VCA 应用扫描二维码。扫描一次即可完成配对；使用 DNSPilot 时还会同时登记远程访问。在终端中：",
    "s3.t": "选卡片，开口说",
    "s3.d": "选择会话，口述内容，确认目标和文字后发送。",

    "m.eyebrow": "早期实测",
    "m.title": "来自真实设备的数据。",
    "m.intro": "里程碑 M0 于 2026 年 10 月在 Windows、macOS、Linux 和一部 iPhone 上逐一验证了各个组件。部分结果如下：",
    "m.s1": "在局域网和 Tailscale 上各连续测试 2 小时，全部请求均得到响应，无一丢失",
    "m.v2": "1.3<small> 秒</small>",
    "m.s2": "服务器重启后重新连接所需时间，无需重新配对",
    "m.v3": "~0.3<small> 秒</small>",
    "m.s3": "在 GPU Whisper 服务器上转写约 10 秒语音所需时间",
    "m.v4": "5.5<small> ms</small>",
    "m.s4": "经加密通道的局域网往返延迟中位数",
    "m.c1": "已在 Windows 和 macOS 上验证受管会话",
    "m.c2": "真实的 Claude Code 钩子和 Codex 输入",
    "m.c3": "用真实 cargo 构建验证准入控制",
    "m.c4": "从 iPhone 语音输入到转写全程验证",
    "m.note": "2026 年 10 月，在 0.1.0 发布之前，于首批测试版本上测得。<a href=\"" + REPORT + "\">阅读完整测试报告（英文）</a>",
    "m.tts": "语音合成引擎另行做了对比：七个开源引擎，用韩语、英语、日语和西班牙语朗读相同的句子。<a href=\"" + TTS + "\">阅读 TTS 基准测试（英文）</a>",

    "p.eyebrow": "兼容性",
    "p.title": "继续用你熟悉的智能体。",
    "p.agentsTitle": "智能体",
    "p.agentsBody": "任何终端智能体都可以作为受管会话运行。Claude Code 和 Codex 有专用适配器；其他智能体使用通用适配器，从输出中识别提示和等待状态。",
    "p.more": "更多",
    "p.legend": "专用适配器",
    "p.tm": "产品名称是其各自所有者的商标。",
    "p.platTitle": "平台",
    "p.platBody": "服务器运行在你的开发电脑上，应用运行在桌面和手机上。桌面安装包可在 GitHub Releases 下载，Android 应用已在 Google Play 上架，iPhone 应用正在 App Store 审核中。",
    "p.kindDesk": "服务器和桌面应用",
    "p.kindMac": "服务器和桌面应用 · 已签名并公证",
    "p.kindApp": "移动应用",
    "p.src": "从源码构建",
    "p.internal": "内部测试版",

    "t.eyebrow": "动手试试",
    "t.title": "也可以从源码构建。",
    "t.body": "从源码安装服务器和 CLI，启动服务器，然后在 VCA 下运行你的第一个智能体。",
    "t.reqs": "需要 Git 和 Rust 工具链。在 Windows 上还需要“使用 C++ 的桌面开发”构建工具。",
    "t.copy": "复制",
    "t.copied": "已复制",
    "t.codeLabel": "快速开始命令",

    "o.eyebrow": "开源",
    "o.title": "MIT 许可证。所有功能，全部免费。",
    "o.intro": "核心用 Rust 编写，应用基于 Tauri 2 构建。所有功能都无需付费。",
    "o.c1.t": "阅读代码",
    "o.c1.d": "服务器、CLI、共享 UI 以及桌面和移动应用都在同一个 MIT 许可的仓库中。",
    "o.c1.l": "浏览仓库",
    "o.c2.t": "参与贡献",
    "o.c2.d": "代码、智能体适配器、翻译，或你所用语言的开发者词汇表。欢迎提交 issue 和 pull request。",
    "o.c2.l": "提交 issue",
    "o.c3.t": "支持这个项目",
    "o.c3.d": "所有功能都无需付费。如果想支持我们，可以考虑用 <a href=\"" + DNSPILOT + "\">DNSPilot</a> 作为连接方式：它由 VCA 的同一团队开发，付费方案开放后，订阅费用将帮助 VCA 的开发。它完全是可选的，与 Tailscale、Cloudflare Tunnel、局域网和自定义方式享有同等支持。你也可以参与贡献，或者给仓库点个 Star 并分享出去。",
    "o.c3.l": "给仓库点 Star",
    "o.credits": "VCA 建立在众多开源项目的成果之上。感谢所有为之付出的人。",
    "o.creditsLink": "开源致谢（英文）",
    "o.noticesLink": "第三方声明（英文）",

    "footer.label": "页脚菜单",
    "footer.slogan": "你的声音。所有智能体。一个指挥中心。",
    "footer.guide": "用户指南",
    "footer.report": "早期测试报告",
    "footer.brand": "品牌指南",
    "footer.credits": "开源致谢",
    "footer.license": "MIT 许可证",
    "footer.privacy": "无追踪、无统计分析、无 Cookie。",
    "nav.download": "下载",
    "f16.t": "屏幕布局",
    "f16.d": "按显示器整理电脑上已打开的终端窗口：1、2、4、6、8 或 10 格网格，也可以自定行数和列数。支持自动填充、保存布局并再次应用，以及撤销上一次移动。",
    "f17.t": "应用锁",
    "f17.d": "应用打开时处于锁定状态，用设备自身的验证方式解锁：Windows Hello、Touch ID、Face ID、指纹或设备密码。锁定期间不显示会话内容，也无法发送任何内容。闲置 1 到 60 分钟后会再次锁定。",
    "f18.t": "常被听错的词",
    "f18.d": "把听写总是写错的词告诉它：填写正确的词以及被误写成的样子。列表保存在你的电脑上，从下一次听写起，对所有已连接的设备生效。",
    "p.kindWin": "服务器和桌面应用 · 由 MetaVu Co., Ltd. 签名",
    "p.rel": "GitHub Releases",
    "p.appStore": "App Store · 审核中",
    "p.play": "Google Play",
    "dl.eyebrow": "下载",
    "dl.title": "获取 Voice Command AI 0.1.1。",
    "dl.intro": "在运行智能体的电脑上安装桌面应用。它包含 VCA 服务器和 vca 命令，并从 GitHub Releases 自动更新；在你点击之前不会安装任何东西。然后在应用的 Settings › Connection & security › Connect a phone 中用一个二维码配对手机。",
    "dl.win": "安装程序（-setup.exe），由 MetaVu Co., Ltd. 签名",
    "dl.mac": "磁盘映像（.dmg），已签名并公证",
    "dl.linux": "提供适用于 x86_64 和 ARM64 的 .deb 和 .rpm 软件包。",
    "dl.cta": "在 GitHub Releases 查看全部文件和发布说明",
    "footer.privacyLink": "隐私政策（英文）",
    "footer.support": "支持（英文）",
    "dl.btnWin": "Windows (x64)",
    "dl.btnMacArm": "macOS（Apple 芯片）",
    "dl.btnMacIntel": "macOS（Intel）",
    "dl.btnLinux": "Linux（x86_64、ARM64）",
    "v.title": "观看 2 分钟演示视频",
    "v.meta": "YouTube 视频（2:10，英文）。在新标签页中打开。",
    "v.url": "https://youtu.be/y1nxSgDrQi8"
  };

  var RU = {
    "meta.title": "Voice Command AI · Ваш голос. Все агенты. Единый командный центр.",
    "meta.desc": "Открытый инструмент: следите за AI-агентами для программирования на своём компьютере и управляйте ими голосом с телефона. Claude Code, Codex, Gemini CLI и другие. Сквозное шифрование, лицензия MIT.",
    "meta.ogTitle": "Voice Command AI: Ваш голос. Все агенты. Единый командный центр.",
    "meta.ogDesc": "Управляйте голосом с телефона всеми AI-агентами для программирования на своём компьютере. Открытый код, лицензия MIT.",

    "skip": "Перейти к содержимому",
    "brand.home": "Voice Command AI, главная",
    "nav.label": "Основное меню",
    "nav.why": "Зачем VCA",
    "nav.features": "Возможности",
    "nav.how": "Как это работает",
    "nav.measured": "Замеры",
    "nav.oss": "Открытый код",
    "lang.label": "Язык",
    "gh.aria": "Voice Command AI на GitHub",

    "hero.symbolAlt": "Символ Voice Command AI",
    "hero.badge": "Версия 0.1.1 · Теперь и для Linux",
    "hero.title": "<span class=\"line\">Ваш голос.</span> <span class=\"line\">Все агенты.</span> <span class=\"line\">Единый командный центр.</span>",
    "hero.lead": "Управляйте голосом с телефона всеми AI-агентами для программирования на своём компьютере.",
    "hero.sub": "Лёгкий сервер на вашем компьютере ведёт все сессии агентов. Телефон показывает их карточками, а каждая команда попадает в нужную сессию, не перехватывая фокус окон.",
    "cta.github": "Открыть на GitHub",
    "cta.guide": "Руководство (англ.)",
    "hero.meta1": "Лицензия MIT",
    "hero.meta2": "Сквозное шифрование",
    "hero.meta3": "Работает на вашем компьютере, без облака VCA",

    "g.alt": "Иллюстрация приложения VCA: три карточки сессий и надиктованная команда для сессии api-server",
    "g.conn": "Подключено · зашифровано",
    "g.run": "Выполняется",
    "g.wait": "Ждёт ввода",
    "g.done": "Готово",
    "g.msg1": "Запускаю набор тестов…",
    "g.msg2": "Применить миграцию базы данных?",
    "g.msg3": "Тесты завершены. Проверьте 2 упавших.",
    "g.to": "Кому",
    "g.text": "Запусти интеграционные тесты и кратко опиши падения.",
    "g.send": "Отправить",
    "g.res": "Интеграционные тесты пройдены. 2 падения описаны.",
    "g.caption": "Иллюстрация. Целевая сессия показывается до отправки.",

    "why.eyebrow": "Зачем VCA",
    "why.title": "Агентов много. Клавиатура одна. Что-то ломается.",
    "why.intro": "Параллельные сессии агентов приводят на одну машину субагентов, компиляторы, раннеры тестов и вспомогательные окна. Первыми ломаются три вещи.",
    "why.with": "С VCA",
    "why.p1.title": "Кража фокуса",
    "why.p1.body": "Новые окна постоянно перехватывают фокус, и промпты с нажатиями клавиш уходят не в тот терминал.",
    "why.p1.fix": "Команды идут прямо в целевую сессию через виртуальный терминал. Никаких имитированных нажатий.",
    "why.p2.title": "Нехватка ресурсов",
    "why.p2.body": "Агенты запускают сборки и тесты, не зная друг о друге, пока процессор и память не забиты и машина не встаёт.",
    "why.p2.fix": "Брокер ресурсов пускает тяжёлые задачи в заданное число слотов, а остальные ставит в очередь.",
    "why.p3.title": "Лишняя навигация",
    "why.p3.body": "Приложения для удалённого управления заставляют ради каждой команды заходить в проект, выходить и заходить в следующий.",
    "why.p3.fix": "Все сессии лежат в одной колоде карточек. Выберите, скажите, отправьте.",

    "feat.eyebrow": "Возможности",
    "feat.title": "Что VCA умеет уже сейчас",
    "feat.intro": "Всё перечисленное ниже входит в версию 0.1.1, кроме пункта с пометкой «Скоро».",
    "soon": "Скоро",
    "f1.t": "Карточки сессий",
    "f1.d": "Каждая сессия агента на вашем компьютере в виде живой карточки: проект, агент, статус и оповещения с первого взгляда. Импортируйте уже открытые сессии AI CLI или запустите новую в выбранной папке. Карточка также показывает, сколько субагентов запустила её сессия.",
    "f2.t": "Сначала голос",
    "f2.d": "Нажмите и говорите. Текст диктовки появляется по ходу речи; проверьте его и отправьте. Движок словаря разработчика сохраняет технические термины без искажений.",
    "f3.t": "Фокус не нужен",
    "f3.d": "Команды попадают в целевую сессию через виртуальный терминал или собственный канал агента, а не через имитацию нажатий.",
    "f4.t": "Живой просмотр",
    "f4.d": "Следите за любой сессией в виде удобного чата или исходного терминала и продолжайте разговор прямо оттуда. Поиск по чату, таблицы Markdown и история, которая подгружается постранично.",
    "f5.t": "Оповещения, точные по желанию",
    "f5.d": "Отдельное меню оповещений со счётчиком собирает все сессии, ждущие решения или ввода. Необязательные хуки агентов, которые вы ставите из настроек, делают оповещения об одобрении точными. Ответ одним касанием.",
    "f6.t": "Голосовые оповещения вашим движком",
    "f6.d": "Слушайте оповещения через свой OpenAI-совместимый движок TTS, например эталонный сервер для MeloTTS или Supertonic. Выберите голос на ПК, и всё будет озвучиваться им, по одному сообщению за раз, с быстрым включением и выключением. Голос устройства подменяет его, только пока звук с ПК не начался.",
    "f7.t": "Задачи от агентов",
    "f7.d": "Когда агенту нужно, чтобы вы вошли в систему, что-то одобрили или запустили, просьба попадает в один список. Отметьте её выполненной и отправьте результат обратно. Команды никогда не запускаются за вас: задачи с правами администратора открывают повышенную оболочку только через системный запрос ОС.",
    "f8.t": "Окна возвращаются после перезагрузки",
    "f8.d": "После перезагрузки или отключения питания VCA снова открывает окна AI CLI на прежнем месте и в том же разговоре. Windows, macOS, Linux и tmux.",
    "f9.t": "Удалённая оболочка под замком",
    "f9.d": "Если агент застрял, откройте оболочку на компьютере с телефона. Это могут только устройства с правами администратора; каждая оболочка требует Face ID или другой биометрии и свежего кода из аутентификатора и закрывается сама по истечении лимита времени.",
    "f10.t": "Сделано для телефона",
    "f10.d": "Листайте сессии в карусели, прикрепляйте фото, файлы или быстрый набросок и выбирайте светлую, тёмную или полностью чёрную тему. Ещё нет ПК? Попробуйте демо-режим с примерами сессий, без сети. Интерфейс приложения на английском, корейском, японском и испанском.",
    "f11.t": "Брокер ресурсов",
    "f11.d": "Процессор, память, диск и сеть по каждой сессии. Тяжёлые задачи пропускаются, ставятся в очередь, ограничиваются или приостанавливаются по вашим правилам.",
    "f12.t": "Сопряжение и шифрование",
    "f12.d": "Одноразовые ссылки для сопряжения, двухфакторный вход по TOTP, короткоживущие токены, сквозное шифрование Noise, журнал аудита и аварийная остановка.",
    "f13.t": "Ваши машины, ваше подключение",
    "f13.d": "Подключите домашний, рабочий и серверный компьютеры к одному приложению. Доступ через прямую LAN, Tailscale, Cloudflare Tunnel, DNSPilot или собственного провайдера. Пока вы не выберете способ, сервер слушает только localhost.",
    "f14.t": "Свои движки",
    "f14.d": "Распознавание речи (например, сервер Whisper), LLM для исправлений и синтез речи вы выбираете сами: локально или в своём облачном аккаунте. VCA не поставляет ни речевых серверов, ни весов моделей.",
    "f15.t": "Уведомления на телефон при закрытом приложении",
    "f15.d": "Запланировано, в этот релиз не входит. Сейчас телефоны показывают уведомления, пока приложение VCA открыто, а на десктопе системные уведомления приходят и в фоне.",

    "c.eyebrow": "Командный центр",
    "c.title": "Дирижёр для всех ваших агентов.",
    "c.intro": "Включите необязательную сессию-дирижёр и спросите: «какие сессии ждут меня?» или «скажи сессии api запустить тесты». Она отвечает по тому, что видит VCA, и действует через MCP-инструменты VCA, но только после вашего разрешения.",
    "c.l1": "Каждое сообщение в другую сессию ждёт вашего «Разрешить» или «Отклонить».",
    "c.l2": "Он никогда не отвечает на запросы разрешений. Это решаете вы.",
    "c.l3": "Число действий ограничено в час, по умолчанию 20.",
    "c.l4": "Аварийная остановка выключает его и отклоняет все ожидающие запросы.",
    "c.l5": "Каждое действие записывается в журнал аудита.",
    "c.runs": "Работает на",
    "c.builtin": "Встроенный, с вашей LLM",
    "c.note": "По умолчанию выключен и включается только в настольном приложении на самом компьютере.",
    "c.link": "Как работает дирижёр (англ.)",
    "c.alt": "Иллюстрация экрана «Командный пункт»: дирижёр просит отправить сообщение в сессию api-server и ждёт «Разрешить» или «Отклонить».",
    "c.p.title": "Командный пункт",
    "c.p.state": "Ждёт вашего ответа",
    "c.p.you": "Скажи api-server запустить интеграционные тесты.",
    "c.p.req": "Дирижёр хочет отправить в <strong>api-server</strong>:",
    "c.p.msg": "Запусти интеграционные тесты и кратко опиши падения.",
    "c.p.allow": "Разрешить",
    "c.p.deny": "Отклонить",
    "c.p.timer": "Без ответа запрос отклоняется через 2 минуты",
    "c.p.cap": "Действий за час",
    "c.p.kill": "Аварийная остановка",

    "how.eyebrow": "Как это работает",
    "how.title": "Телефон общается только с вашим компьютером.",
    "how.intro": "После однократного сопряжения приложение подключается к серверу VCA на вашем компьютере по каналу Noise со сквозным шифрованием. Сессиями агентов управляет сервер. Ничего не проходит через облако VCA.",
    "d.alt": "Архитектура: приложение VCA на телефоне или компьютере подключается по каналу со сквозным шифрованием к серверу VCA на вашей машине. Сервер управляет агентами, такими как Claude Code, Codex, Gemini CLI, Antigravity и Grok, и может использовать ваши собственные движки распознавания речи, LLM и синтеза речи.",
    "d.app": "Приложение VCA",
    "d.appSub": "Телефон · компьютер",
    "d.app1": "Карточки сессий",
    "d.app2": "Голосовая диктовка",
    "d.app3": "Живой просмотр и оповещения",
    "d.e2e": "Сквозное шифрование",
    "d.noise": "Протокол Noise",
    "d.via1": "LAN · Tailscale · Cloudflare",
    "d.via2": "DNSPilot · свой",
    "d.server": "Сервер VCA",
    "d.serverSub": "на вашем компьютере",
    "d.s1": "Менеджер сессий (PTY)",
    "d.s2": "Адаптеры агентов",
    "d.s3": "Брокер ресурсов",
    "d.s4": "Речевой конвейер",
    "d.s5": "Аутентификация · аудит · API",
    "d.agents": "Агенты на вашем компьютере",
    "d.more": "и другие",
    "d.stt": "Ваши речевые движки (необяз.)",
    "d.sttSub": "STT · LLM · TTS, совместимые с OpenAI",
    "d.caption": "Синей рамкой отмечены агенты со своим адаптером. Остальные работают через универсальный.",
    "s1.t": "Запускайте агентов через VCA",
    "s1.d": "Управляемые сессии принимают ввод из приложения, не трогая фокус окон.",
    "s2.t": "Сопряжение один раз",
    "s2.d": "В настольном приложении откройте Settings › Connection & security › Connect a phone и отсканируйте QR-код приложением VCA. Одно сканирование выполняет сопряжение, а с DNSPilot ещё и регистрирует телефон для удалённого доступа. Из терминала:",
    "s3.t": "Выберите карточку и говорите",
    "s3.d": "Выберите сессию, надиктуйте, проверьте адресата и текст и отправьте.",

    "m.eyebrow": "Первые замеры",
    "m.title": "Цифры с реальных машин.",
    "m.intro": "На этапе M0 в октябре 2026 года каждый компонент проверили на Windows, macOS, Linux и iPhone. Несколько результатов:",
    "m.s1": "запросов получили ответ за 2-часовой прогон по LAN и по Tailscale, без единой потери",
    "m.v2": "1,3<small> с</small>",
    "m.s2": "на переподключение после перезапуска сервера, без повторного сопряжения",
    "m.v3": "~0,3<small> с</small>",
    "m.s3": "на распознавание примерно 10 с речи на GPU-сервере Whisper",
    "m.v4": "5,5<small> мс</small>",
    "m.s4": "медианная задержка туда и обратно по LAN через зашифрованный канал",
    "m.c1": "Управляемые сессии проверены на Windows и macOS",
    "m.c2": "Реальные хуки Claude Code и ввод в Codex",
    "m.c3": "Контроль допуска на реальных сборках cargo",
    "m.c4": "От голосового ввода на iPhone до текста",
    "m.note": "Измерено на первых тестовых сборках в октябре 2026 года, до выхода версии 0.1.0. <a href=\"" + REPORT + "\">Читать полный отчёт о тестах (англ.)</a>",
    "m.tts": "Движки синтеза речи сравнили отдельно: семь открытых движков, одни и те же фразы на корейском, английском, японском и испанском. <a href=\"" + TTS + "\">Читать сравнение TTS (англ.)</a>",

    "p.eyebrow": "Совместимость",
    "p.title": "Работайте с привычными агентами.",
    "p.agentsTitle": "Агенты",
    "p.agentsBody": "Любой терминальный агент можно запустить как управляемую сессию. У Claude Code и Codex есть свои адаптеры; остальные работают через универсальный адаптер, который читает из вывода вопросы и состояния ожидания.",
    "p.more": "и другие",
    "p.legend": "Свой адаптер",
    "p.tm": "Названия продуктов являются товарными знаками их владельцев.",
    "p.platTitle": "Платформы",
    "p.platBody": "Сервер работает на вашем рабочем компьютере, приложение — на десктопе и телефоне. Установщики для десктопа лежат в GitHub Releases, приложение для Android доступно в Google Play, а приложение для iPhone проходит проверку в App Store.",
    "p.kindDesk": "Сервер и настольное приложение",
    "p.kindMac": "Сервер и настольное приложение · подписано и нотаризовано",
    "p.kindApp": "Мобильное приложение",
    "p.src": "Сборка из исходников",
    "p.internal": "Внутренние тестовые сборки",

    "t.eyebrow": "Попробуйте",
    "t.title": "Или соберите из исходников.",
    "t.body": "Установите сервер и CLI из исходников, запустите сервер и первого агента через VCA.",
    "t.reqs": "Нужны Git и инструментарий Rust. В Windows также средства сборки «Разработка классических приложений на C++».",
    "t.copy": "Копировать",
    "t.copied": "Скопировано",
    "t.codeLabel": "Команды быстрого старта",

    "o.eyebrow": "Открытый код",
    "o.title": "Лицензия MIT. Все функции бесплатно.",
    "o.intro": "Ядро написано на Rust, приложения собраны на Tauri 2. Всеми функциями можно пользоваться, никому не платя.",
    "o.c1.t": "Читайте код",
    "o.c1.d": "Сервер, CLI, общий интерфейс и приложения для компьютера и телефона лежат в одном репозитории под лицензией MIT.",
    "o.c1.l": "Открыть репозиторий",
    "o.c2.t": "Участвуйте",
    "o.c2.d": "Код, адаптеры агентов, переводы или словарь разработчика для вашего языка. Issue и pull request приветствуются.",
    "o.c2.l": "Создать issue",
    "o.c3.t": "Поддержать проект",
    "o.c3.d": "Всеми функциями можно пользоваться бесплатно. Если хотите помочь, рассмотрите <a href=\"" + DNSPILOT + "\">DNSPilot</a> для подключения: его делает та же команда, что и VCA, и когда откроются платные тарифы, подписки помогут финансировать VCA. Это полностью необязательно: Tailscale, Cloudflare Tunnel, LAN и собственные провайдеры поддерживаются наравне. Ещё можно участвовать в разработке или поставить репозиторию звезду и рассказать о нём.",
    "o.c3.l": "Поставить звезду",
    "o.credits": "VCA построен на труде многих открытых проектов. Спасибо всем, кто их создаёт.",
    "o.creditsLink": "Благодарности открытым проектам (англ.)",
    "o.noticesLink": "Уведомления о сторонних компонентах (англ.)",

    "footer.label": "Меню в подвале",
    "footer.slogan": "Ваш голос. Все агенты. Единый командный центр.",
    "footer.guide": "Руководство",
    "footer.report": "Отчёт о первых тестах",
    "footer.brand": "Брендбук",
    "footer.credits": "Благодарности открытым проектам",
    "footer.license": "Лицензия MIT",
    "footer.privacy": "Без отслеживания, без аналитики, без cookie.",
    "nav.download": "Скачать",
    "f16.t": "Раскладка окон",
    "f16.d": "Расставьте уже открытые окна терминалов на ПК по каждому монитору: сетка на 1, 2, 4, 6, 8 или 10 ячеек или свои строки и столбцы. Автозаполнение, сохранённые раскладки, которые можно применить снова, и отмена последнего перемещения.",
    "f17.t": "Блокировка приложения",
    "f17.d": "Приложения открываются заблокированными и разблокируются средствами самого устройства: Windows Hello, Touch ID, Face ID, отпечаток пальца или код-пароль. Пока приложение заблокировано, содержимое сессий не видно и ничего нельзя отправить. Оно снова блокируется через 1–60 минут бездействия.",
    "f18.t": "Часто неверно распознаваемые слова",
    "f18.d": "Подскажите диктовке слова, в которых она постоянно ошибается: правильное слово и то, как оно записывается по ошибке. Список хранится на ПК, поэтому со следующей диктовки он работает на всех подключённых устройствах.",
    "p.kindWin": "Сервер и настольное приложение · подписано MetaVu Co., Ltd.",
    "p.rel": "GitHub Releases",
    "p.appStore": "App Store · на проверке",
    "p.play": "Google Play",
    "dl.eyebrow": "Скачать",
    "dl.title": "Скачайте Voice Command AI 0.1.1.",
    "dl.intro": "Установите настольное приложение на ПК, где работают ваши агенты. В него входят сервер VCA и команда vca, а обновляется оно само из GitHub Releases; ничего не устанавливается, пока вы не нажмёте кнопку. Затем подключите телефон одним QR-кодом в разделе Settings › Connection & security › Connect a phone.",
    "dl.win": "Установщик (-setup.exe), подписан MetaVu Co., Ltd.",
    "dl.mac": "Образ диска (.dmg), подписан и нотаризован.",
    "dl.linux": "Пакеты .deb и .rpm для x86_64 и ARM64.",
    "dl.cta": "Все файлы и примечания к выпуску в GitHub Releases",
    "footer.privacyLink": "Конфиденциальность (англ.)",
    "footer.support": "Поддержка (англ.)",
    "dl.btnWin": "Windows (x64)",
    "dl.btnMacArm": "macOS (Apple silicon)",
    "dl.btnMacIntel": "macOS (Intel)",
    "dl.btnLinux": "Linux (x86_64, ARM64)",
    "v.title": "Смотреть 2-минутный обзор",
    "v.meta": "Видео на YouTube (2:10, на английском). Откроется в новой вкладке.",
    "v.url": "https://youtu.be/y1nxSgDrQi8"
  };

  var DICTS = { ko: KO, ja: JA, es: ES, zh: ZH, ru: RU };
  var EN_EXTRA = { "t.copied": "Copied" };
  var EN = {};
  var current = "en";

  var textNodes = document.querySelectorAll("[data-i18n]");
  var attrNodes = document.querySelectorAll("[data-i18n-attr]");

  // Capture the English source from the markup once.
  textNodes.forEach(function (el) {
    var key = el.getAttribute("data-i18n");
    if (!(key in EN)) EN[key] = el.innerHTML;
  });
  attrNodes.forEach(function (el) {
    parseAttr(el).forEach(function (pair) {
      if (!(pair.key in EN)) EN[pair.key] = el.getAttribute(pair.attr);
    });
  });
  Object.keys(EN_EXTRA).forEach(function (k) { EN[k] = EN_EXTRA[k]; });

  function parseAttr(el) {
    return el.getAttribute("data-i18n-attr").split(";").map(function (part) {
      var i = part.indexOf(":");
      return { attr: part.slice(0, i).trim(), key: part.slice(i + 1).trim() };
    });
  }

  function t(key) {
    var dict = DICTS[current] || EN;
    return key in dict ? dict[key] : EN[key];
  }

  function apply(lang) {
    lang = String(lang || "").toLowerCase().slice(0, 2);
    current = LANGS.indexOf(lang) > -1 ? lang : "en";
    document.documentElement.lang = HTML_LANG[current] || current;

    textNodes.forEach(function (el) {
      var value = t(el.getAttribute("data-i18n"));
      if (value == null) return;
      // SVG text and <title> take plain text; HTML elements may carry inline markup from our own dictionary.
      if (el instanceof SVGElement || el.tagName === "TITLE") el.textContent = value;
      else el.innerHTML = value;
    });
    attrNodes.forEach(function (el) {
      parseAttr(el).forEach(function (pair) {
        var value = t(pair.key);
        if (value != null) el.setAttribute(pair.attr, value);
      });
    });

    document.querySelectorAll(".lang-toggle button").forEach(function (b) {
      b.setAttribute("aria-pressed", String(b.getAttribute("data-lang") === current));
    });
    var sel = document.querySelector(".lang-select select");
    if (sel) sel.value = current;

    var canonical = document.querySelector('link[rel="canonical"]');
    if (canonical) canonical.href = "https://vca.metavu.io/" + (current === "en" ? "" : "?lang=" + current);
  }

  function save(lang) {
    try { localStorage.setItem(STORAGE_KEY, lang); } catch (e) { /* storage may be unavailable */ }
  }

  function syncUrl(lang) {
    try {
      var url = new URL(location.href);
      if (lang === "en") url.searchParams.delete("lang");
      else url.searchParams.set("lang", lang);
      history.replaceState(null, "", url.pathname + url.search + url.hash);
    } catch (e) { /* ignore */ }
  }

  // The head script already resolved the language (URL > saved choice > browser).
  apply(document.documentElement.lang);

  function choose(lang) {
    if (lang === current) return;
    apply(lang);
    save(lang);
    syncUrl(lang);
    if (onLang) onLang();
  }
  // Wide screens: a row of buttons. Narrow screens: a compact native dropdown with the same choices.
  document.querySelectorAll(".lang-toggle button").forEach(function (b) {
    b.addEventListener("click", function () { choose(b.getAttribute("data-lang")); });
  });
  var langSelect = document.querySelector(".lang-select select");
  if (langSelect) langSelect.addEventListener("change", function () { choose(langSelect.value); });

  // Copy button for the quick start block.
  var status = document.getElementById("copy-status");
  document.querySelectorAll(".copy-btn").forEach(function (btn) {
    if (!navigator.clipboard) { btn.hidden = true; return; }
    btn.addEventListener("click", function () {
      var src = document.querySelector(btn.getAttribute("data-copy"));
      if (!src) return;
      // Strip the "$ " prompts so the commands paste cleanly.
      var text = src.textContent.split("\n").map(function (line) { return line.replace(/^\$ /, ""); }).join("\n");
      navigator.clipboard.writeText(text).then(function () {
        btn.textContent = t("t.copied");
        if (status) status.textContent = t("t.copied");
        setTimeout(function () {
          btn.textContent = t("t.copy");
          if (status) status.textContent = "";
        }, 1800);
      }, function () { /* clipboard denied: leave the button as is */ });
    });
  });

  /* ---------- Motion ----------
     Brand motion (docs/brand/brand-definition.md §8): recording moves the waveform, sending is one
     converging motion, an idle logo stays still, reduced motion is respected. Everything pauses
     off-screen and in hidden tabs. Without JS the page is fully visible and static. */
  var root = document.documentElement;
  var mq = window.matchMedia ? matchMedia("(prefers-reduced-motion: reduce)") : null;
  var hasIO = "IntersectionObserver" in window;
  var STROKES = '<use href="#vs1"/><use href="#vs2"/><use href="#vs3"/><use href="#vs4"/><use href="#vs5"/>';
  var onLang = null;
  function still() { return !!(mq && mq.matches); }
  function live() { return !document.hidden && !still(); }
  // Calls cb(visible) whenever el enters or leaves the viewport (always visible without IntersectionObserver).
  function watch(el, cb, threshold) {
    if (!el) return;
    if (!hasIO) { cb(true); return; }
    new IntersectionObserver(function (es) { cb(es[es.length - 1].isIntersecting); }, { threshold: threshold || 0 }).observe(el);
  }

  // Hero entrance is pure CSS; afterwards freeze it so language swaps don't replay it.
  setTimeout(function () { root.classList.add("intro-done"); }, still() ? 0 : 1300);

  // The logo reacts like a voice on hover or tap (event-driven only, ~1.2 s, then the exact resting shape).
  var sym = document.querySelector(".hero-symbol");
  function talk() {
    if (!sym || still() || !root.classList.contains("intro-done") || sym.classList.contains("is-talk")) return;
    sym.classList.add("is-talk");
    setTimeout(function () { sym.classList.remove("is-talk"); }, 1450);
  }
  if (sym) { sym.addEventListener("pointerenter", talk); sym.addEventListener("click", talk); }
  // Keyboard users: focusing the header logo link makes the symbol listen too.
  var brand = document.querySelector(".site-header .brand");
  if (brand) brand.addEventListener("focus", talk);

  var hero = document.querySelector(".hero");
  var heroVis = true;
  function heroUpdate() { if (hero) hero.classList.toggle("is-off", !(heroVis && live())); }
  watch(hero, function (v) { heroVis = v; heroUpdate(); });

  // The waiting request in the Command illustration only breathes while it is on screen.
  var cpanel = document.querySelector(".c-panel");
  var cpanelVis = false;
  function cpanelUpdate() { if (cpanel) cpanel.classList.toggle("is-off", !(cpanelVis && live())); }
  cpanelUpdate();
  watch(cpanel, function (v) { cpanelVis = v; cpanelUpdate(); });

  // Language toggle: one pill slides between the buttons.
  var toggle = document.querySelector(".lang-toggle");
  function placePill() {
    var b = toggle && toggle.querySelector('[aria-pressed="true"]');
    if (!b) return;
    toggle.style.setProperty("--px", b.offsetLeft + "px");
    toggle.style.setProperty("--pw", b.offsetWidth + "px");
    toggle.classList.add("has-pill");
  }
  if (toggle) {
    placePill();
    requestAnimationFrame(function () { requestAnimationFrame(function () { toggle.classList.add("pill-ready"); }); });
    if (document.fonts) document.fonts.ready.then(placePill);
    window.addEventListener("resize", placePill, { passive: true });
  }

  // Section labels carry a small five-stroke mark (re-added if a language swap rewrites the label).
  function marks() {
    document.querySelectorAll(".eyebrow").forEach(function (eb) {
      if (eb.querySelector(".eb-mark")) return;
      eb.insertAdjacentHTML("afterbegin", '<svg class="eb-mark" viewBox="168 168 842 737" fill="url(#heroGrad)" aria-hidden="true" focusable="false">' + STROKES + "</svg>");
      eb.classList.add("has-mark");
    });
  }
  marks();

  // Illustration story (~9.5 s loop): dictation types in while the waveform moves, Send presses,
  // the strokes converge, the target card lights up and turns Done, then everything resets.
  var gl = document.querySelector(".glimpse");
  var story = gl && (function () {
    var text = gl.querySelector(".g-text"), send = gl.querySelector(".g-send"), card = gl.querySelector(".g-card.is-target");
    var timers = [], running = false, vis = false, first = true;
    function at(ms, fn) { timers.push(setTimeout(fn, ms)); }
    function clear() { timers.forEach(clearTimeout); timers = []; }
    function reset() {
      gl.classList.remove("is-rec", "is-send", "is-sent", "is-live");
      send.classList.remove("is-press");
      card.classList.remove("is-hit", "is-done");
      text.textContent = t("g.text");
    }
    function cycle() {
      clear();
      reset();
      gl.classList.add("is-live");
      var full = Array.from(t("g.text")), n = full.length, step = Math.min(70, 3400 / n);
      text.innerHTML = '<span class="g-typed"></span><span class="g-rest"></span>';
      var typed = text.firstChild, rest = text.lastChild;
      rest.textContent = full.join("");
      at(400, function () {
        gl.classList.add("is-rec");
        if (first) { first = false; talk(); }
      });
      full.forEach(function (_, i) {
        at(650 + (i + 1) * step, function () {
          typed.textContent = full.slice(0, i + 1).join("");
          rest.textContent = full.slice(i + 1).join("");
        });
      });
      var s = 650 + n * step + 450;
      at(s, function () { send.classList.add("is-press"); });
      at(s + 180, function () { send.classList.remove("is-press"); gl.classList.remove("is-rec"); gl.classList.add("is-send"); });
      at(s + 750, function () { gl.classList.add("is-sent"); card.classList.add("is-hit"); });
      at(s + 1050, function () { card.classList.add("is-done"); });
      at(s + 2300, function () { card.classList.remove("is-hit"); });
      at(s + 4300, function () { gl.classList.remove("is-send"); card.classList.remove("is-done"); });
      at(s + 5000, cycle);
    }
    function update() {
      var go = vis && live();
      if (go && !running) { running = true; clear(); at(first ? 1000 : 300, cycle); }
      else if (!go && running) { running = false; clear(); reset(); }
    }
    watch(gl, function (v) { vis = v; update(); }, 0.25);
    return { update: update, lang: function () { if (running) cycle(); } };
  })();

  // Architecture diagram: data pulses and dashes only run while the diagram is on screen.
  var dia = document.querySelector(".diagram"), diaVis = false;
  function diaUpdate() {
    if (!dia) return;
    var go = diaVis && live();
    dia.classList.toggle("is-paused", !go);
    dia.querySelectorAll("svg").forEach(function (s) {
      if (s.pauseAnimations) { if (go) s.unpauseAnimations(); else s.pauseAnimations(); }
    });
  }
  diaUpdate();
  watch(dia, function (v) { diaVis = v; diaUpdate(); });

  function refresh() { heroUpdate(); cpanelUpdate(); diaUpdate(); if (story) story.update(); }
  document.addEventListener("visibilitychange", refresh);
  if (mq && mq.addEventListener) mq.addEventListener("change", refresh);

  onLang = function () { marks(); placePill(); if (story) story.lang(); };

  if (!hasIO || still()) return;

  // Scroll reveals: once per element, small stagger inside grids. Classes are removed afterwards
  // so hover transitions take over again.
  var rvIO = new IntersectionObserver(function (es) {
    es.forEach(function (e) {
      if (!e.isIntersecting) return;
      var el = e.target, m = el.querySelector(".eb-mark");
      rvIO.unobserve(el);
      el.classList.add("in");
      if (m) m.classList.add("play");
      setTimeout(function () { el.classList.remove("rv", "in"); el.style.removeProperty("--rv-d"); }, 1100);
    });
  }, { rootMargin: "0px 0px -8% 0px", threshold: 0.1 });
  document.querySelectorAll(".section-head, .glimpse-caption, .problem, .feature, .c-rules, .c-panel, .diagram, .step, .stat, .verified, .measured-note, .split > div, .code, .oss-card, .oss-credits, .dl-item, .try-head").forEach(function (el) {
    if (el.tagName === "LI") el.style.setProperty("--rv-d", (Array.prototype.indexOf.call(el.parentNode.children, el) % 4) * 0.07 + "s");
    el.classList.add("rv");
    rvIO.observe(el);
  });

  // Measured numbers count up once from 0. Screen readers get the exact final text throughout.
  // "7,199" uses a thousands comma; "1,3" (Spanish) uses a decimal comma.
  var stats = document.querySelectorAll(".stat-value"), runs = [];
  stats.forEach(function (el) {
    var html = el.innerHTML, m = html.match(/^(~?)(\d+(?:,\d{3})*)(?:([.,])(\d+))?/);
    if (!m) return;
    var dec = m[4] ? m[4].length : 0, sep = m[3] || ".", comma = m[2].indexOf(",") > -1;
    var target = parseFloat(m[2].replace(/,/g, "") + (m[4] ? "." + m[4] : ""));
    var fmt = function (v) {
      var parts = v.toFixed(dec).split(".");
      if (comma) parts[0] = parts[0].replace(/\B(?=(\d{3})+(?!\d))/g, ",");
      return parts.join(sep);
    };
    var final = el.textContent;
    el.innerHTML = '<span aria-hidden="true"><span class="cu"></span>' + html.slice(m[0].length) + '</span><span class="visually-hidden"></span>';
    el.lastChild.textContent = final;
    var cu = el.querySelector(".cu");
    cu.textContent = m[1] + fmt(0);
    runs.push(function () {
      var t0 = performance.now();
      requestAnimationFrame(function frame(now) {
        var p = Math.min(1, (now - t0) / 1400), e = 1 - Math.pow(1 - p, 3);
        cu.textContent = m[1] + fmt(target * e);
        if (p < 1) requestAnimationFrame(frame);
        else { var k = el.getAttribute("data-i18n"); el.innerHTML = k ? t(k) : html; }
      });
    });
  });
  var statsIO = new IntersectionObserver(function (es) {
    if (!es[0].isIntersecting) return;
    statsIO.disconnect();
    runs.forEach(function (r) { r(); });
  }, { threshold: 0.35 });
  if (runs.length) statsIO.observe(document.querySelector(".stats"));
})();
