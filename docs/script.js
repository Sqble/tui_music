document.addEventListener('DOMContentLoaded', () => {
    const sidebar = document.getElementById('sidebar');
    const toggle = document.getElementById('sidebarToggle');
    const navItems = document.querySelectorAll('.nav-item:not(.external)');
    const sections = document.querySelectorAll('.docs-section');

    function showSection(sectionId) {
        sections.forEach(section => {
            section.classList.remove('active');
            if (section.id === sectionId) {
                section.classList.add('active');
            }
        });

        navItems.forEach(item => {
            item.classList.remove('active');
            if (item.dataset.section === sectionId) {
                item.classList.add('active');
            }
        });

        if (window.innerWidth <= 768) {
            sidebar.classList.remove('open');
        }
    }

    toggle.addEventListener('click', () => {
        sidebar.classList.toggle('open');
    });

    navItems.forEach(item => {
        item.addEventListener('click', (e) => {
            e.preventDefault();
            const sectionId = item.dataset.section;
            showSection(sectionId);
            window.location.hash = sectionId;
        });
    });

    function handleHashChange() {
        const hash = window.location.hash.slice(1);
        if (hash && document.getElementById(hash)) {
            showSection(hash);
        } else if (!hash) {
            showSection('landing');
        }
    }

    window.addEventListener('hashchange', handleHashChange);
    handleHashChange();

    document.querySelectorAll('a[href^="#"]').forEach(anchor => {
        anchor.addEventListener('click', (e) => {
            const targetId = anchor.getAttribute('href').slice(1);
            if (document.getElementById(targetId)) {
                e.preventDefault();
                showSection(targetId);
                window.location.hash = targetId;
            }
        });
    });
});
